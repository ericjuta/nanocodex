use std::{path::PathBuf, time::Duration};

use eyre::{Result, eyre};
use nanocodex_core::ResponseItem;
use serde_json::Value;

use super::{
    CellUpdate, CodeModeExecution, LiveCell, NestedToolCall, nested_tool_yield_after, observe_cell,
    observer_yield_timeout, parse_exec_source,
};
use crate::{ToolContext, ToolOutputBody, ToolOutputContent, ToolRuntime, WebSearchConfig};

#[test]
fn long_observer_yields_include_completion_grace() {
    assert_eq!(
        observer_yield_timeout(Duration::from_secs(10)),
        Duration::from_secs(11)
    );
    assert_eq!(
        observer_yield_timeout(Duration::from_millis(9_999)),
        Duration::from_millis(9_999)
    );
}

#[tokio::test]
async fn prewarms_embedded_cljrs_host() -> Result<()> {
    let workspace = temporary_workspace("prewarmed-cljrs-host")?;
    let runtime = super::CodeModeRuntime::new(workspace.clone());

    assert!(runtime.host.lock().await.host.is_some());

    runtime.control().terminate_all().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn execution_definitions_do_not_leak_across_cljrs_namespaces() -> Result<()> {
    let workspace = temporary_workspace("isolated-cljrs-namespaces")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let context = test_context(&history);
    let source = r"(do
  (def generation 1)
  (text {:previous nil :current generation}))";

    let first = tools.execute_code(source, context).await;
    let second = tools.execute_code(source, context).await;

    assert!(first.success, "{}", execution_output(&first));
    assert!(second.success, "{}", execution_output(&second));
    assert_eq!(
        serde_json::from_str::<Value>(emitted_text(&first)?)?,
        serde_json::json!({"previous": null, "current": 1})
    );
    assert_eq!(
        serde_json::from_str::<Value>(emitted_text(&second)?)?,
        serde_json::json!({"previous": null, "current": 1})
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn restricted_cljrs_blocks_ambient_file_io() -> Result<()> {
    let workspace = temporary_workspace("restricted-cljrs-file-io")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(r#"(slurp "/etc/passwd")"#, test_context(&history))
        .await;

    assert!(!execution.success);
    assert!(execution_output(&execution).contains("forbidden"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn read_errors_report_exact_cell_locations() -> Result<()> {
    let workspace = temporary_workspace("read-diagnostic")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code("(do\n  (text 1)\n", test_context(&history))
        .await;

    let output = execution_output(&execution);
    assert!(!execution.success);
    assert!(output.contains("read error:"), "{output}");
    assert!(output.contains("at <nanocodex.cell.1>:"), "{output}");
    assert!(output.contains("exact reader location"), "{output}");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn eval_errors_report_form_location_class_and_ex_data() -> Result<()> {
    let workspace = temporary_workspace("eval-diagnostic")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            "(text \"before\")\n(throw (ex-info \"boom\" {:kind :diagnostic :n 7}))",
            test_context(&history),
        )
        .await;

    let output = execution_output(&execution);
    assert!(!execution.success);
    assert!(output.contains("thrown error:"), "{output}");
    assert!(output.contains("top-level form: 2"), "{output}");
    assert!(output.contains("enclosing top-level form"), "{output}");
    assert!(output.contains("\"kind\":\"diagnostic\""), "{output}");
    assert!(output.contains("\"n\":7"), "{output}");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn execution_local_bindings_do_not_leak_across_cljrs_calls() -> Result<()> {
    let workspace = temporary_workspace("scoped-cljrs-bindings")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let context = test_context(&history);
    let source = r"(let [execution-local 1] (text execution-local))";

    let first = tools.execute_code(source, context).await;
    let second = tools.execute_code(source, context).await;

    assert!(first.success, "{}", execution_output(&first));
    assert!(second.success, "{}", execution_output(&second));
    assert_eq!(emitted_text(&first)?, "1");
    assert_eq!(emitted_text(&second)?, "1");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn restricted_cljrs_blocks_namespace_loading() -> Result<()> {
    let workspace = temporary_workspace("restricted-cljrs-namespaces")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(r"(require 'clojure.string)", test_context(&history))
        .await;

    assert!(!execution.success);
    assert!(execution_output(&execution).contains("forbidden"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn multiple_yielded_cells_continue_and_complete_independently() -> Result<()> {
    let workspace = temporary_workspace("multiple-live-cells")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let first = tools
        .execute_code(
            r#"(do
  (await (yield-control))
  (let [result (await (nanocodex.tools/call "exec_command"
                  {:cmd "sleep 0.04; printf 'first done'" :login false}))]
    (text (:output result))))"#,
            test_context_with_call(&history, "call-first"),
        )
        .await;
    let second = tools
        .execute_code(
            r#"(do
  (await (yield-control))
  (let [result (await (nanocodex.tools/call "exec_command"
                  {:cmd "sleep 0.01; printf 'second done'" :login false}))]
    (text (:output result))))"#,
            test_context_with_call(&history, "call-second"),
        )
        .await;

    assert!(execution_output(&first).contains("Script running with cell ID 1"));
    assert!(execution_output(&second).contains("Script running with cell ID 2"));

    let second = tools
        .wait_for_code(
            r#"{"cell_id":"2","yield_time_ms":1000}"#,
            test_context_with_call(&history, "call-wait-second"),
        )
        .await;
    let first = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":1000}"#,
            test_context_with_call(&history, "call-wait-first"),
        )
        .await;

    assert!(second.success, "{}", execution_output(&second));
    assert!(execution_output(&second).contains("second done"));
    assert_eq!(second.nested_calls.len(), 1);
    assert!(first.success, "{}", execution_output(&first));
    assert!(execution_output(&first).contains("first done"));
    assert_eq!(first.nested_calls.len(), 1);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn join_all_runs_nested_tools_concurrently() -> Result<()> {
    let workspace = temporary_workspace("parallel-nested-tools")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [first (nanocodex.tools/call "exec_command"
                    {:cmd "touch first.started; i=0; while [ \"$i\" -lt 100 ]; do [ -f second.started ] && exit 0; i=$((i + 1)); sleep 0.01; done; exit 91"
                     :login false})
       second (nanocodex.tools/call "exec_command"
                     {:cmd "touch second.started; i=0; while [ \"$i\" -lt 100 ]; do [ -f first.started ] && exit 0; i=$((i + 1)); sleep 0.01; done; exit 92"
                      :login false})
       results (await (clojure.core.async/join-all [first second]))]
  (text {:first (:exit_code (nth results 0))
         :second (:exit_code (nth results 1))}))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(
        call_ids(&execution.nested_calls),
        ["call-exec/code-1", "call-exec/code-2"]
    );
    let result = serde_json::from_str::<Value>(emitted_text(&execution)?)?;
    assert_eq!(result, serde_json::json!({ "first": 0, "second": 0 }));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn failed_nested_tool_rejects_its_clojure_future() -> Result<()> {
    let workspace = temporary_workspace("nested-tool-rejection")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(try
  (await (tool-call "view_image" {:path "missing.png"}))
  (text "unexpected success")
  (catch :default error
    (text {:message (ex-message error) :data (ex-data error)})))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let failure = serde_json::from_str::<Value>(emitted_text(&execution)?)?;
    assert!(
        failure["message"]
            .as_str()
            .is_some_and(|message| message.contains("unable to locate image"))
    );
    assert_eq!(failure["data"]["type"], "nested-tool-failure");
    assert_eq!(failure["data"]["tool"], "view_image");
    assert_eq!(
        failure["data"]["input"],
        serde_json::json!({"path": "missing.png"})
    );
    assert_eq!(failure["data"]["call-id"], "call-exec/code-1");
    assert!(
        failure["data"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("unable to locate image"))
    );
    assert_eq!(execution.nested_calls.len(), 1);
    assert!(!execution.nested_calls[0].success);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn unknown_nested_tool_failure_suggests_close_names() -> Result<()> {
    let workspace = temporary_workspace("nested-tool-suggestion")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(try
  (await (tool-call "exec_comand" {:cmd "pwd"}))
  (text "unexpected success")
  (catch :default error
    (text (ex-message error))))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let message = emitted_text(&execution)?;
    assert!(
        message.contains("Did you mean `exec_command`?"),
        "{message}"
    );
    assert!(message.contains("(all-tools)"), "{message}");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn tool_call_alias_uses_the_standard_nested_call_lifecycle() -> Result<()> {
    let workspace = temporary_workspace("tool-call-alias")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [result (await (tool-call "exec_command" {:cmd "printf alias" :login false}))]
  (text (:output result)))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(emitted_text(&execution)?, "alias");
    assert_eq!(execution.nested_calls.len(), 1);
    assert_eq!(execution.nested_calls[0].name, "exec_command");
    assert_eq!(execution.nested_calls[0].call_id, "call-exec/code-1");
    assert_eq!(
        execution.nested_calls[0].input,
        serde_json::json!({"cmd": "printf alias", "login": false})
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn image_helper_requires_data_urls() -> Result<()> {
    let workspace = temporary_workspace("code-mode-image-urls")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();

    let remote = tools
        .execute_code(
            r#"(image "https://example.com/image.png")"#,
            test_context(&history),
        )
        .await;
    assert!(!remote.success);
    assert!(execution_output(&remote).contains("remote image URLs are not supported"));

    let invalid = tools
        .execute_code(r#"(image "not-an-image")"#, test_context(&history))
        .await;
    assert!(!invalid.success);
    assert!(execution_output(&invalid).contains("invalid image output"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn failed_cell_preserves_accumulated_output() -> Result<()> {
    let workspace = temporary_workspace("failed-cell-output")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(do
  (text "before crash")
  (image "data:image/png;base64,a" "original")
  (throw (ex-info "boom" {})))"#,
            test_context(&history),
        )
        .await;

    assert!(!execution.success);
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    assert!(matches!(
        content.get(1),
        Some(ToolOutputContent::InputText { text }) if text == "before crash"
    ));
    assert!(matches!(
        content.get(2),
        Some(ToolOutputContent::InputImage {
            image_url,
            detail: crate::ImageDetail::Original,
        }) if image_url == "data:image/png;base64,a"
    ));
    assert!(matches!(
        content.get(3),
        Some(ToolOutputContent::InputText { text }) if text.contains("boom")
    ));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn image_helper_normalizes_detail_and_honors_override() -> Result<()> {
    let workspace = temporary_workspace("code-mode-image-detail")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(image {:image_url "data:image/png;base64,a" :detail "low"} "ORIGINAL")"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    assert!(matches!(
        content.last(),
        Some(ToolOutputContent::InputImage {
            image_url,
            detail: crate::ImageDetail::Original,
        }) if image_url == "data:image/png;base64,a"
    ));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn generated_image_helper_appends_high_detail_image_and_hint() -> Result<()> {
    let workspace = temporary_workspace("code-mode-generated-image")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(generated-image
  {:image_url "data:image/png;base64,a"
   :output_hint "generated image save hint"})"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    assert!(matches!(
        content.get(1),
        Some(ToolOutputContent::InputImage {
            image_url,
            detail: crate::ImageDetail::High,
        }) if image_url == "data:image/png;base64,a"
    ));
    assert!(matches!(
        content.get(2),
        Some(ToolOutputContent::InputText { text }) if text == "generated image save hint"
    ));

    let invalid = tools
        .execute_code(
            r#"(generated-image {:image_url "data:image/png;base64,a" :output_hint 1})"#,
            test_context(&history),
        )
        .await;
    assert!(!invalid.success);
    assert!(
        execution_output(&invalid)
            .contains("generated-image output_hint must be a string when provided")
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn notify_serializes_values_and_rejects_empty_text() -> Result<()> {
    let workspace = temporary_workspace("code-mode-notify")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(do (notify {:phase "working"}) (text "done"))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(execution.notifications.len(), 1);
    assert_eq!(execution.notifications[0].call_id, "call-exec");
    assert_eq!(
        serde_json::from_str::<Value>(&execution.notifications[0].text)?,
        serde_json::json!({"phase": "working"})
    );

    let empty = tools
        .execute_code(r#"(notify "  ")"#, test_context(&history))
        .await;
    assert!(!empty.success);
    assert!(execution_output(&empty).contains("notify expects non-empty text"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn store_round_trips_json_compatible_clojure_values() -> Result<()> {
    let workspace = temporary_workspace("code-mode-store-json")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let write = tools
        .execute_code(
            r#"(store "candidate" {:kept 1 :array [nil nil]})"#,
            test_context(&history),
        )
        .await;
    assert!(write.success, "{}", execution_output(&write));

    let read = tools
        .execute_code(
            r#"(text (load "candidate"))"#,
            test_context_with_call(&history, "call-read"),
        )
        .await;
    assert!(read.success, "{}", execution_output(&read));
    assert_eq!(
        serde_json::from_str::<Value>(emitted_text(&read)?)?,
        serde_json::json!({ "kept": 1, "array": [null, null] })
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn store_rejects_non_serializable_values_at_the_call_boundary() -> Result<()> {
    let workspace = temporary_workspace("code-mode-store-errors")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(r#"(store "candidate" (fn [] nil))"#, test_context(&history))
        .await;

    assert!(!execution.success);
    assert!(
        execution_output(&execution).contains("unsupported Clojure value at the tool boundary")
    );

    let read = tools
        .execute_code(
            r#"(text (load "candidate"))"#,
            test_context_with_call(&history, "call-read"),
        )
        .await;
    assert!(read.success, "{}", execution_output(&read));
    assert_eq!(emitted_text(&read)?, "null");

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn exit_completes_the_cell_successfully() -> Result<()> {
    let workspace = temporary_workspace("code-mode-exit")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(do
  (text (count (all-tools)))
  (exit)
  (throw (ex-info "unreachable" {})))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert!(
        emitted_text(&execution)?.parse::<usize>()? > 0,
        "all-tools should expose enabled tool metadata"
    );
    assert!(!execution_output(&execution).contains("unreachable"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn tool_introspection_filters_and_returns_schemas() -> Result<()> {
    let workspace = temporary_workspace("tool-introspection")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [matches (all-tools "hashline__read")
      info (tool-info :exec_command)]
  (text {:count (count matches)
         :name (:name info)
         :input-type (:type (:input_schema info))
         :dynamic (:dynamic info)}))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let info = serde_json::from_str::<Value>(emitted_text(&execution)?)?;
    assert!(
        info["count"].as_u64().is_some_and(|count| count >= 1),
        "filtered introspection should return at least the named tool: {info}"
    );
    assert_eq!(info["name"], "exec_command");
    assert_eq!(info["input-type"], "object");
    assert_eq!(info["dynamic"], false);
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn yielded_cell_completes_through_wait() -> Result<()> {
    let workspace = temporary_workspace("yielded-cell")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(do
  (text "before")
  (await (yield-control))
  (await (clojure.core.async/timeout 10))
  (text "after"))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success);
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));
    assert!(execution_output(&execution).contains("before"));

    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":1000}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success, "{}", execution_output(&completed));
    assert!(execution_output(&completed).contains("Script completed"));
    assert!(execution_output(&completed).contains("after"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn explicit_yield_allows_pending_nested_tools() -> Result<()> {
    let workspace = temporary_workspace("yield-pending-tool")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [call (tool-call "exec_command"
                         {:cmd "printf Cargo.toml" :login false})]
  (text {:pending (count (pending-tools))})
  (await (yield-control))
  (text (:output (await call))))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));
    assert!(execution_output(&execution).contains(r#"{"pending":1}"#));
    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":3000}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success, "{}", execution_output(&completed));
    assert!(
        execution_output(&completed).contains("Cargo.toml"),
        "{}",
        execution_output(&completed)
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancel_tool_aborts_one_pending_branch_and_keeps_siblings() -> Result<()> {
    let workspace = temporary_workspace("cancel-pending-tool")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [slow (tool-call "exec_command" {:cmd "sleep 5" :login false})
      fast (tool-call "exec_command" {:cmd "printf fast" :login false})
      pending (count (pending-tools))
      cancelled (cancel-tool slow)
      result (await fast)]
  (text {:pending pending :cancelled cancelled :output (:output result)}))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let result = serde_json::from_str::<Value>(emitted_text(&execution)?)?;
    assert_eq!(
        result,
        serde_json::json!({
            "pending": 2,
            "cancelled": true,
            "output": "fast"
        })
    );
    assert_eq!(execution.nested_calls.len(), 2);
    let cancelled = execution
        .nested_calls
        .iter()
        .find(|call| call.call_id == "call-exec/code-1")
        .expect("cancelled call should be recorded");
    assert!(!cancelled.success);
    assert!(matches!(
        &cancelled.output,
        ToolOutputBody::Text(output) if output.contains("cancelled")
    ));
    assert!(
        execution
            .nested_calls
            .iter()
            .any(|call| call.call_id == "call-exec/code-2" && call.success)
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn running_shell_session_survives_output_only_clojure() -> Result<()> {
    let workspace = temporary_workspace("running-shell-session-output")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [result (await (nanocodex.tools/call "exec_command"
                         {:cmd "sleep 5" :yield_time_ms 250}))]
  (text (:output result)))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert!(
        execution_output(&execution)
            .contains("Nested shell process is still running with session ID 1")
    );
    tools.control().cancel().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn running_shell_session_notice_is_not_duplicated_for_full_results() -> Result<()> {
    let workspace = temporary_workspace("visible-running-shell-session")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [result (await (nanocodex.tools/call "exec_command"
                         {:cmd "sleep 5" :yield_time_ms 250}))]
  (text result))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    let output = execution_output(&execution);
    assert!(output.contains(r#""session_id":1"#));
    assert!(!output.contains("Nested shell process is still running"));
    tools.control().cancel().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_terminates_yielded_code_cells() -> Result<()> {
    let workspace = temporary_workspace("cancelled-cell")?;
    let tools = test_tools(&workspace);
    let control = tools.control();
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r"(do
  (await (yield-control))
  (await (clojure.core.async/take! (clojure.core.async/chan))))",
            test_context(&history),
        )
        .await;
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));

    tokio::time::timeout(std::time::Duration::from_secs(2), control.cancel()).await?;
    let missing = tools
        .wait_for_code(r#"{"cell_id":"1"}"#, test_context(&history))
        .await;
    assert!(!missing.success);
    assert!(execution_output(&missing).contains("exec cell 1 was not found"));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn cancellation_interrupts_busy_clojure_and_recreates_the_host() -> Result<()> {
    let workspace = temporary_workspace("cancelled-busy-cell")?;
    let tools = test_tools(&workspace);
    let control = tools.control();
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r"(do
  (await (yield-control))
  (loop [] (recur)))",
            test_context(&history),
        )
        .await;
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));

    tokio::time::timeout(std::time::Duration::from_secs(2), control.cancel()).await?;
    let recovered = tools
        .execute_code(r#"(text "recovered")"#, test_context(&history))
        .await;

    assert!(recovered.success, "{}", execution_output(&recovered));
    assert_eq!(emitted_text(&recovered)?, "recovered");
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_drops_pending_tool_futures_before_recreating_the_host() -> Result<()> {
    let workspace = temporary_workspace("cancelled-pending-tool")?;
    let tools = test_tools(&workspace);
    let control = tools.control();
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#";; @exec: {"yield_time_ms": 10}
(await (nanocodex.tools/call "exec_command" {:cmd "sleep 5" :login false}))"#,
            test_context(&history),
        )
        .await;
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));

    tokio::time::timeout(std::time::Duration::from_secs(2), control.cancel()).await?;
    let recovered = tools
        .execute_code(r#"(text "recovered")"#, test_context(&history))
        .await;

    assert!(recovered.success, "{}", execution_output(&recovered));
    assert_eq!(emitted_text(&recovered)?, "recovered");
    tools.control().cancel().await;
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn resumed_cell_notifications_keep_the_original_exec_call_id() -> Result<()> {
    let workspace = temporary_workspace("resumed-cell-notify")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(do
  (await (yield-control))
  (notify "after yield")
  (text "done"))"#,
            test_context_with_call(&history, "call-original-exec"),
        )
        .await;

    assert!(execution.success);
    assert!(execution.notifications.is_empty());

    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"1","yield_time_ms":1000}"#,
            test_context_with_call(&history, "call-wait"),
        )
        .await;
    assert!(completed.success, "{}", execution_output(&completed));
    assert_eq!(completed.notifications.len(), 1);
    assert_eq!(completed.notifications[0].call_id, "call-original-exec");
    assert_eq!(completed.notifications[0].text, "after yield");

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn hashline_workspace_tools_are_callable_from_code_mode() -> Result<()> {
    let workspace = temporary_workspace("hashline-workspace-tools")?;
    std::fs::write(workspace.join("notes.txt"), "alpha\nbeta\n")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            r#"(let [read (await (nanocodex.tools/call "hashline__read" {:path "notes.txt"}))]
  (await (nanocodex.tools/call "hashline__find_block"
           {:path "notes.txt" :anchor "1:93c8"}))
  (await (nanocodex.tools/call "hashline__patch"
           {:header (:patchHeader read)
            :operations "SWAP 2:f589:\n+bravo"
            :dry_run true}))
  (await (nanocodex.tools/call "hashline__transaction"
           {:action {:type "preview"}
            :mutations [{:type "create" :path "new.txt" :contents "new\n"}]}))
  (text "done"))"#,
            test_context(&history),
        )
        .await;

    assert!(execution.success, "{}", execution_output(&execution));
    assert_eq!(emitted_text(&execution)?, "done");
    assert_eq!(
        execution
            .nested_calls
            .iter()
            .map(|call| call.name.as_str())
            .collect::<Vec<_>>(),
        [
            "hashline__read",
            "hashline__find_block",
            "hashline__patch",
            "hashline__transaction",
        ]
    );
    assert!(execution.nested_calls.iter().all(|call| call.success));
    assert_eq!(
        std::fs::read_to_string(workspace.join("notes.txt"))?,
        "alpha\nbeta\n"
    );

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[tokio::test]
async fn exec_pragma_and_wait_limit_direct_output() -> Result<()> {
    let workspace = temporary_workspace("code-output-limits")?;
    let tools = test_tools(&workspace);
    let history = Vec::new();
    let execution = tools
        .execute_code(
            ";; @exec: {\"max_output_tokens\": 2}\n(text \"abcdefghijklmnop\")",
            test_context(&history),
        )
        .await;
    assert!(execution.success);
    assert!(execution_output(&execution).contains("Warning: truncated output"));

    let yielded = tools
        .execute_code(
            r#"(do
  (await (yield-control))
  (text "abcdefghijklmnop"))"#,
            test_context(&history),
        )
        .await;
    assert!(yielded.success);
    let completed = tools
        .wait_for_code(
            r#"{"cell_id":"2","yield_time_ms":1000,"max_tokens":2}"#,
            test_context(&history),
        )
        .await;
    assert!(completed.success);
    assert!(execution_output(&completed).contains("Warning: truncated output"));
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn exec_pragma_rejects_unknown_fields() {
    let error = parse_exec_source(";; @exec: {\"unknown\": 1}\n(text \"hi\")")
        .err()
        .expect("unknown pragma fields should fail");
    assert!(error.contains("only supports"));
}

#[test]
fn nested_shell_yields_follow_the_handlers_bounds() {
    assert_eq!(
        nested_tool_yield_after(
            "exec_command",
            &serde_json::json!({ "yield_time_ms": 45_000 }),
        ),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        nested_tool_yield_after(
            "write_stdin",
            &serde_json::json!({ "session_id": 1, "yield_time_ms": 120_000 }),
        ),
        Some(Duration::from_secs(120))
    );
    assert_eq!(
        nested_tool_yield_after(
            "write_stdin",
            &serde_json::json!({
                "session_id": 1,
                "chars": "x",
                "yield_time_ms": 120_000,
            }),
        ),
        Some(Duration::from_secs(30))
    );
    assert_eq!(
        nested_tool_yield_after(
            "view_image",
            &serde_json::json!({ "yield_time_ms": 30_000 })
        ),
        None
    );
}

#[tokio::test]
async fn default_cell_yield_extends_for_a_longer_nested_shell_wait() {
    let (updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, _terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        updates_tx
            .send(CellUpdate::NestedCallStarted {
                name: "write_stdin".to_owned(),
                yield_after: Duration::from_millis(40),
            })
            .expect("observer should receive the nested call");
        tokio::time::sleep(Duration::from_millis(15)).await;
        updates_tx
            .send(CellUpdate::Completed {
                content: Vec::new(),
            })
            .expect("observer should receive cell completion");
    });
    let mut cell = LiveCell {
        id: 1,
        output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
        updates,
        terminate: Some(terminate),
        task: Some(task),
    };

    let (execution, running) = observe_cell(
        &mut cell,
        std::time::Instant::now(),
        Duration::from_millis(5),
        None,
        true,
    )
    .await;

    assert!(!running);
    assert!(execution.success);
    assert!(execution_output(&execution).contains("Script completed"));
    cell.join().await;
}

#[tokio::test]
async fn explicit_cell_yield_is_not_extended_by_a_nested_shell_wait() {
    let (updates_tx, updates) = tokio::sync::mpsc::unbounded_channel();
    let (terminate, _terminate_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        updates_tx
            .send(CellUpdate::NestedCallStarted {
                name: "write_stdin".to_owned(),
                yield_after: Duration::from_millis(40),
            })
            .expect("observer should receive the nested call");
        tokio::time::sleep(Duration::from_millis(15)).await;
        let _ = updates_tx.send(CellUpdate::Completed {
            content: Vec::new(),
        });
    });
    let mut cell = LiveCell {
        id: 1,
        output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
        updates,
        terminate: Some(terminate),
        task: Some(task),
    };

    let (execution, running) = observe_cell(
        &mut cell,
        std::time::Instant::now(),
        Duration::from_millis(5),
        None,
        false,
    )
    .await;

    assert!(running);
    assert!(execution.success);
    assert!(execution_output(&execution).contains("Script running with cell ID 1"));
    cell.join().await;
}

#[test]
fn model_description_uses_clojure_declarations() {
    let workspace = temporary_workspace("code-mode-description")
        .expect("temporary test workspace should be available");
    let tools = test_tools(&workspace);
    let specs = tools
        .model_specs()
        .into_iter()
        .map(|spec| serde_json::to_value(spec).unwrap())
        .collect::<Vec<_>>();
    let description = specs[0]["description"]
        .as_str()
        .expect("exec should have a description");
    let static_guide_bytes = description
        .split_once("\n\n### `")
        .map_or(description.len(), |(guide, _)| guide.len());
    let input_schema_bytes = description
        .lines()
        .filter_map(|line| line.strip_prefix("Detailed input schema (JSON): "))
        .map(str::len)
        .sum::<usize>();
    let output_schema_bytes = description
        .lines()
        .filter_map(|line| line.strip_prefix("Detailed output schema (JSON): "))
        .map(str::len)
        .sum::<usize>();
    assert!(
        description.len() <= 39_000,
        "Code Mode description exceeded its 39,000-byte budget: total={}, static_guide={static_guide_bytes}, input_schemas={input_schema_bytes}, output_schemas={output_schema_bytes}",
        description.len()
    );
    assert!(description.contains(";; @exec:"));
    assert!(description.contains("base64 `data:` URI"));
    assert!(!description.contains("apply_patch"));
    assert!(description.contains(r#"(tool-call "hashline__read""#));
    assert!(description.contains(r#"(tool-call "exec_command""#));
    assert!(description.contains("equivalent namespaced primitive"));
    assert!(description.contains("(tool-info name)"));
    assert!(description.contains("(pending-tools)"));
    assert!(description.contains("(cancel-tool future)"));
    assert!(description.contains(":nested-tool-failure"));
    assert!(description.contains("Detailed input schema (JSON):"));
    assert!(
        !description.contains("Detailed output schema (JSON): unspecified JSON-compatible value")
    );
    assert_eq!(
        specs[1]["parameters"]["properties"]["max_tokens"]["type"],
        "number"
    );
    std::fs::remove_dir_all(workspace).expect("temporary workspace should be removable");
}

fn emitted_text(execution: &CodeModeExecution) -> Result<&str> {
    let ToolOutputBody::Content(content) = &execution.output else {
        return Err(eyre!("code-mode execution did not emit content"));
    };
    content
        .iter()
        .rev()
        .find_map(|item| match item {
            ToolOutputContent::InputText { text } => Some(text.as_str()),
            ToolOutputContent::InputImage { .. } => None,
        })
        .ok_or_else(|| eyre!("code-mode execution did not emit text"))
}

fn execution_output(execution: &CodeModeExecution) -> String {
    match &execution.output {
        ToolOutputBody::Text(text) => text.clone(),
        ToolOutputBody::Content(content) => content
            .iter()
            .filter_map(|item| match item {
                ToolOutputContent::InputText { text } => Some(text.as_str()),
                ToolOutputContent::InputImage { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn call_ids(calls: &[NestedToolCall]) -> Vec<&str> {
    calls.iter().map(|call| call.call_id.as_str()).collect()
}

fn test_tools(workspace: &std::path::Path) -> ToolRuntime {
    ToolRuntime::new(
        workspace,
        Some(WebSearchConfig {
            endpoint: "http://127.0.0.1:1/v1/alpha/search".to_owned(),
            auth: nanocodex_core::OpenAiAuth::api_key("test-key"),
        }),
        Some(super::super::ImageGenerationConfig {
            api_base_url: "http://127.0.0.1:1/v1".to_owned(),
            auth: nanocodex_core::OpenAiAuth::api_key("test-key"),
            save_root: workspace.to_path_buf(),
        }),
    )
}

fn test_context(history: &[ResponseItem]) -> ToolContext<'_> {
    test_context_with_call(history, "call-exec")
}

fn test_context_with_call<'a>(history: &'a [ResponseItem], call_id: &'a str) -> ToolContext<'a> {
    ToolContext {
        model: "test-model",
        session_id: "test-session",
        call_id,
        history,
        output_token_budget: crate::DEFAULT_TOOL_OUTPUT_TOKENS,
    }
}

fn temporary_workspace(label: &str) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "nanocodex-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
