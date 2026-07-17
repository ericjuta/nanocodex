type NodeProps = {
  eyebrow?: string;
  label: string;
  meta?: string;
  tone?: "default" | "public" | "terminal" | "error";
  className?: string;
};

function Node({ eyebrow, label, meta, tone = "default", className = "" }: NodeProps) {
  return (
    <div className={`map-node map-node--${tone} ${className}`}>
      {eyebrow ? <span>{eyebrow}</span> : null}
      <strong>{label}</strong>
      {meta ? <small>{meta}</small> : null}
    </div>
  );
}

function Arrow({ direction = "right", dotted = false, className = "" }: { direction?: "right" | "down" | "left"; dotted?: boolean; className?: string }) {
  return <span className={`map-arrow map-arrow--${direction}${dotted ? " is-dotted" : ""} ${className}`} aria-hidden="true" />;
}

export function Architecture() {
  return (
    <section className="architecture-diagram-page" aria-labelledby="architecture-title">
      <header className="diagram-page-header page-grid">
        <div>
          <p className="eyebrow">Architecture</p>
          <h1 id="architecture-title">Rust runtime callgraph</h1>
        </div>
        <div className="diagram-legend" aria-label="Diagram legend">
          <span><i /> Hot path</span>
          <span><i /> Recovery</span>
          <span><i /> Terminal</span>
        </div>
      </header>

      <div className="system-map-scroll">
        <div className="system-map" role="img" aria-label="The Harness Rust API enters through harness run, validates the request, constructs a ModelRun, connects and warms the model session, then repeats model calls and tool execution until it emits one completed or failed terminal event.">
          <section className="map-lane map-lane--public">
            <p className="map-lane-label">Public API</p>
            <div className="public-api-flow">
              <div className="public-inputs">
                <span>BufRead</span>
                <span>Write</span>
                <span>ModelConfig</span>
              </div>
              <Arrow direction="down" />
              <Node eyebrow="crate boundary" label="harness::run" meta="Result<()>" tone="public" />
            </div>
          </section>

          <div className="lane-arrow" aria-hidden="true"><Arrow direction="down" /></div>

          <section className="map-lane map-lane--setup">
            <p className="map-lane-label">Request setup</p>
            <div className="setup-map-flow">
              <Node eyebrow="validate" label="task.start" meta="read_task_start" />
              <Arrow />
              <Node eyebrow="sequence" label="EventWriter" meta="request_id · seq" />
              <Arrow />
              <Node eyebrow="owner" label="ModelRun" meta="task · config · stats" />
              <Arrow />
              <Node eyebrow="context" label="Workspace + tools" meta="AGENTS.md · ToolRuntime" />
              <Arrow />
              <Node eyebrow="transport" label="Connect + warm up" meta="Responses WebSocket" />
            </div>
          </section>

          <div className="lane-arrow" aria-hidden="true"><Arrow direction="down" /></div>

          <section className="map-lane map-lane--loop">
            <div className="map-lane-title">
              <p className="map-lane-label">Turn state machine</p>
              <span>Repeats</span>
            </div>

            <div className="turn-map">
              <Node eyebrow="request" label="Model call" meta="delta + previous response" className="turn-model" />
              <Arrow className="model-to-stream" />
              <Node eyebrow="receive" label="Stream events" meta="text · reasoning · items" className="turn-stream" />
              <Arrow className="stream-to-decision" />
              <div className="map-decision turn-decision"><span>output?</span></div>
              <div className="branch-arrow branch-arrow--final"><span>message</span><Arrow /></div>
              <Node label="Assistant message" meta="final text" className="turn-message" />
              <Arrow className="message-to-complete" />
              <Node eyebrow="terminal" label="run.completed" tone="terminal" className="turn-complete" />

              <div className="decision-drop"><span>code calls</span><Arrow direction="down" /></div>

              <Node eyebrow="dispatch" label="exec / wait" meta="execute_model_tool" className="tool-dispatch" />
              <Arrow className="dispatch-to-code" />
              <Node eyebrow="cell" label="CodeModeRuntime" meta="persistent JS host" className="tool-code-mode" />
              <Arrow className="code-to-runtime" />
              <div className="map-node tool-runtime">
                <span>nested tools</span>
                <strong>ToolRuntime</strong>
                <div>
                  <i>shell</i>
                  <i>stdin</i>
                  <i>plan</i>
                  <i>patch</i>
                  <i>image</i>
                </div>
              </div>
              <Arrow className="runtime-to-output" />
              <Node eyebrow="result" label="Tool output" meta="custom / function output" className="tool-output" />
              <Arrow className="output-to-state" />
              <Node eyebrow="state" label="History + delta" meta="ConversationState" className="tool-state" />

              <div className="loop-wire" aria-hidden="true"><span>next turn</span></div>
            </div>
          </section>

          <section className="map-lane map-lane--sidepaths">
            <p className="map-lane-label">Side paths</p>
            <div className="sidepath-grid">
              <div className="sidepath-flow">
                <Node eyebrow="send closed" label="Reconnect" />
                <Arrow dotted />
                <Node label="Replay full history" meta="previous_response_id = none" />
                <Arrow dotted />
                <span className="rejoin-pill">rejoin model call ↺</span>
              </div>
              <div className="sidepath-flow">
                <Node eyebrow="context limit" label="Compact" />
                <Arrow dotted />
                <Node label="Install summary" meta="retain user messages" />
                <Arrow dotted />
                <span className="rejoin-pill">rejoin model call ↺</span>
              </div>
            </div>
          </section>

          <section className="map-lane map-lane--terminal">
            <p className="map-lane-label">Failure</p>
            <div className="failure-flow">
              <span>unrecovered error</span>
              <Arrow direction="right" dotted />
              <Node label="run.error" tone="error" />
              <Arrow direction="right" />
              <Node eyebrow="terminal" label="run.failed" tone="error" />
              <span className="terminal-rule">exactly one terminal event</span>
            </div>
          </section>
        </div>
      </div>
    </section>
  );
}
