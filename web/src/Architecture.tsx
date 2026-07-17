const setupSteps = [
  {
    call: "read_task_start",
    detail: "Validate one task.start envelope",
    source: "protocol.rs",
  },
  {
    call: "EventWriter::new",
    detail: "Own request ID + monotonic JSONL sequence",
    source: "protocol.rs",
  },
  {
    call: "model::run",
    detail: "Cross into the model runtime",
    source: "model/mod.rs",
  },
  {
    call: "ModelRun::execute",
    detail: "Own the request lifecycle + terminal event",
    source: "model/agent.rs",
  },
];

const nestedTools = ["exec_command", "write_stdin", "update_plan", "apply_patch", "view_image"];

export function Architecture() {
  return (
    <section className="architecture-page page-grid" aria-labelledby="architecture-title">
      <header className="architecture-hero">
        <div className="architecture-intro">
          <p className="eyebrow">
            <span className="architecture-status" aria-hidden="true" />
            Architecture / Rust runtime
          </p>
          <h1 id="architecture-title">One request. One owner. One loop.</h1>
          <p>
            The public Rust API is deliberately tiny. Everything below it exists to turn one
            validated request into a sequence of observable events, then exactly one terminal
            outcome.
          </p>
        </div>

        <aside className="api-contract" aria-label="Rust public API">
          <header>
            <span>Public boundary</span>
            <span>src/lib.rs</span>
          </header>
          <pre>
            <code>
              <span>pub async fn</span> run(
              {"\n  "}input: impl BufRead,
              {"\n  "}output: impl Write,
              {"\n  "}config: ModelConfig,
              {"\n"}) -&gt; Result&lt;()&gt;
            </code>
          </pre>
          <dl>
            <div>
              <dt>Configuration</dt>
              <dd>ModelConfig · ReasoningEffort</dd>
            </div>
            <div>
              <dt>Failures</dt>
              <dd>HarnessError → AgentError | ResponsesError</dd>
            </div>
          </dl>
        </aside>
      </header>

      <section className="architecture-section" aria-labelledby="callgraph-title">
        <header className="architecture-section-heading">
          <div>
            <p className="rail-label">High-level callgraph</p>
            <h2 id="callgraph-title">The whole runtime on one page</h2>
          </div>
          <p>
            Read top to bottom. The dark path is the hot path; dotted paths recover or reduce
            context, then rejoin the same loop.
          </p>
        </header>

        <div className="callgraph" role="region" aria-label="Callgraph from the public Rust run function through setup, model calls, tool execution, compaction, and terminal events">
          <div className="callgraph-entry flow-card flow-card--public">
            <span className="flow-card-index">Public API</span>
            <strong>harness::run</strong>
            <small>BufRead + Write + ModelConfig</small>
          </div>

          <div className="flow-arrow flow-arrow--down" aria-hidden="true">
            <span />
          </div>

          <section className="setup-stage" aria-labelledby="setup-stage-title">
            <header>
              <span>01</span>
              <div>
                <strong id="setup-stage-title">Accept + construct</strong>
                <small>Everything required becomes owned state near the boundary.</small>
              </div>
            </header>
            <ol className="setup-flow">
              {setupSteps.map((step) => (
                <li key={step.call}>
                  <code>{step.call}</code>
                  <span>{step.detail}</span>
                  <small>{step.source}</small>
                </li>
              ))}
            </ol>
          </section>

          <div className="flow-arrow flow-arrow--down" aria-hidden="true">
            <span />
          </div>

          <section className="runtime-loop" aria-labelledby="runtime-loop-title">
            <header className="runtime-loop-heading">
              <span>02</span>
              <div>
                <strong id="runtime-loop-title">ModelRun owns the state machine</strong>
                <small>Event writer · task · config · timing · stats · turn state</small>
              </div>
              <span className="loop-badge">Repeats per turn</span>
            </header>

            <div className="runtime-setup">
              <div>
                <code>execute_task</code>
                <span>Resolve workspace</span>
                <span>Load AGENTS.md</span>
                <span>Build ToolRuntime + request profile</span>
              </div>
              <span className="flow-arrow flow-arrow--right" aria-hidden="true" />
              <div>
                <code>connect_with_warmup_fallback</code>
                <span>Open Responses WebSocket</span>
                <span>Warm cached prefix</span>
                <span>Fall back to a full first request</span>
              </div>
            </div>

            <div className="turn-spine">
              <article className="flow-card">
                <span className="flow-card-index">1 · request</span>
                <strong>perform_model_call</strong>
                <small>Send delta + previous_response_id</small>
              </article>
              <span className="flow-arrow flow-arrow--right" aria-hidden="true" />
              <article className="flow-card">
                <span className="flow-card-index">2 · stream</span>
                <strong>stream::receive</strong>
                <small>Emit deltas; collect output items</small>
              </article>
              <span className="flow-arrow flow-arrow--right" aria-hidden="true" />
              <article className="flow-card flow-card--decision">
                <span className="flow-card-index">3 · decide</span>
                <strong>TurnResult</strong>
                <small>final_message or code_calls?</small>
              </article>
            </div>

            <div className="turn-branches">
              <section className="terminal-branch" aria-label="Terminal message path">
                <p className="branch-label">No code calls</p>
                <div className="branch-flow">
                  <div>
                    <code>final_message</code>
                    <span>Return assistant text</span>
                  </div>
                  <span className="flow-arrow flow-arrow--right" aria-hidden="true" />
                  <div className="terminal-card">
                    <strong>assistant.message</strong>
                    <span>run.completed</span>
                  </div>
                </div>
              </section>

              <section className="tool-branch" aria-label="Tool execution loop path">
                <p className="branch-label">One or more code calls</p>
                <div className="tool-flow">
                  <div className="tool-flow-step">
                    <code>execute_model_tool</code>
                    <span>Dispatch exec / wait</span>
                  </div>
                  <span className="flow-arrow flow-arrow--right" aria-hidden="true" />
                  <div className="tool-flow-step tool-flow-step--runtime">
                    <code>CodeModeRuntime</code>
                    <span>Run one persistent JS cell</span>
                    <p>
                      {nestedTools.map((tool) => (
                        <span key={tool}>{tool}</span>
                      ))}
                    </p>
                  </div>
                  <span className="flow-arrow flow-arrow--right" aria-hidden="true" />
                  <div className="tool-flow-step">
                    <code>ConversationState</code>
                    <span>Append tool output to history + delta</span>
                  </div>
                  <span className="loop-return" aria-hidden="true">next turn ↺</span>
                </div>
              </section>
            </div>
          </section>

          <div className="runtime-sidepaths">
            <article>
              <p className="rail-label">Recovery path</p>
              <strong>Reconnect → replay full history</strong>
              <span>
                A closed send reconnects, clears previous_response_id, and resends the full input.
              </span>
              <small>Then rejoin perform_model_call</small>
            </article>
            <article>
              <p className="rail-label">Context path</p>
              <strong>Threshold → remote compaction</strong>
              <span>
                Install one compacted item, retain real user messages, and rebuild the next delta.
              </span>
              <small>Then rejoin perform_model_call</small>
            </article>
            <article className="runtime-sidepaths-terminal">
              <p className="rail-label">Failure path</p>
              <strong>Any unrecovered error</strong>
              <span>Emit run.error, then the request’s one terminal run.failed event.</span>
              <small>Return HarnessError</small>
            </article>
          </div>
        </div>
      </section>

      <section className="architecture-invariants" aria-labelledby="invariants-title">
        <header>
          <p className="rail-label">Keep these in your head</p>
          <h2 id="invariants-title">Four runtime invariants</h2>
        </header>
        <ol>
          <li>
            <span>01</span>
            <strong>One request in flight</strong>
            <p>The public call accepts one task.start and owns it until completion.</p>
          </li>
          <li>
            <span>02</span>
            <strong>One lifecycle owner</strong>
            <p>ModelRun carries the socket, context, statistics, timing, and event stream.</p>
          </li>
          <li>
            <span>03</span>
            <strong>Tools feed the same loop</strong>
            <p>Every tool result becomes model input; there is no second orchestration path.</p>
          </li>
          <li>
            <span>04</span>
            <strong>Exactly one terminal event</strong>
            <p>Accepted requests end in run.completed or run.failed—never both.</p>
          </li>
        </ol>
      </section>
    </section>
  );
}
