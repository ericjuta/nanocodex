import { FormEvent, useEffect, useMemo, useRef, useState } from "react";

type Thinking = "low" | "medium" | "high" | "xhigh";
type Status = "waiting" | "starting" | "ready" | "running" | "completed" | "failed";
type JsonObject = Record<string, unknown>;
type AgentEvent = {
  protocol_version: number;
  request_id: string;
  seq: number;
  type: string;
  payload: JsonObject;
};
type TranscriptItem = {
  id: number;
  role: "user" | "assistant" | "error";
  text: string;
};
type ToolTrace = {
  call_id?: string;
  tool?: string;
  arguments?: unknown;
  result?: unknown;
  status?: string;
  duration_ns?: number;
};
type WorkerMessage =
  | { type: "ready" }
  | { type: "event"; event: AgentEvent }
  | { type: "result"; id: number; message: string }
  | { type: "error"; id?: number; message: string };

const DEFAULT_PROMPT = `Inspect the browser runtime with tools.browserInfo(), then explain what is running in Rust/WASM versus JavaScript. End with one concrete idea for a useful browser-native tool.`;

export function App() {
  const workerRef = useRef<Worker | null>(null);
  const nextId = useRef(1);
  const startedAt = useRef(0);
  const [thinking, setThinking] = useState<Thinking>("medium");
  const [status, setStatus] = useState<Status>("waiting");
  const [ready, setReady] = useState(false);
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const [pending, setPending] = useState<Set<number>>(() => new Set());
  const [transcript, setTranscript] = useState<TranscriptItem[]>([]);
  const [events, setEvents] = useState<AgentEvent[]>([]);
  const [liveAnswer, setLiveAnswer] = useState("");
  const [reasoning, setReasoning] = useState("");
  const [elapsedMs, setElapsedMs] = useState(0);

  useEffect(() => {
    const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
    workerRef.current = worker;
    worker.onmessage = ({ data }: MessageEvent<WorkerMessage>) => {
      if (data.type === "ready") {
        setReady(true);
        setStatus("ready");
        return;
      }
      if (data.type === "event") {
        const event = data.event;
        setEvents((current) => [...current.slice(-499), event]);
        if (event.type === "run.started") {
          setStatus("running");
        } else if (event.type === "assistant.delta") {
          setLiveAnswer((current) => current + payloadText(event));
        } else if (event.type === "assistant.message") {
          setLiveAnswer(payloadText(event));
        } else if (event.type === "reasoning.summary.delta") {
          setReasoning((current) => current + payloadText(event));
        } else if (event.type === "run.completed") {
          setStatus("completed");
          setElapsedMs(payloadNumber(event.payload, "duration_ms") ?? Date.now() - startedAt.current);
        } else if (event.type === "run.failed") {
          setStatus("failed");
          setElapsedMs(payloadNumber(event.payload, "duration_ms") ?? Date.now() - startedAt.current);
        } else if (event.type === "run.error") {
          const message = payloadString(event.payload, "message") ?? "The run failed.";
          setTranscript((current) => [...current, { id: nextId.current++, role: "error", text: message }]);
        }
        return;
      }
      if (data.type === "result") {
        setPending((current) => without(current, data.id));
        setLiveAnswer(data.message);
        setTranscript((current) => [
          ...current,
          { id: data.id, role: "assistant", text: data.message },
        ]);
        return;
      }
      if (data.id !== undefined) setPending((current) => without(current, data.id!));
      setStatus("failed");
      setTranscript((current) => [
        ...current,
        { id: data.id ?? nextId.current++, role: "error", text: data.message },
      ]);
    };
    return () => worker.terminate();
  }, []);

  useEffect(() => {
    if (status !== "running") return;
    const timer = window.setInterval(() => setElapsedMs(Date.now() - startedAt.current), 100);
    return () => window.clearInterval(timer);
  }, [status]);

  const terminal = useMemo(
    () => [...events].reverse().find((event) => event.type === "run.completed" || event.type === "run.failed"),
    [events],
  );
  const tools = useMemo(() => toolTimeline(events), [events]);
  const stats = asObject(terminal?.payload.stats);
  const usage = asObject(stats?.usage);

  function start(event: FormEvent) {
    event.preventDefault();
    setReady(false);
    setStatus("starting");
    setPending(new Set());
    setTranscript([]);
    setEvents([]);
    setLiveAnswer("");
    setReasoning("");
    setElapsedMs(0);
    workerRef.current?.postMessage({ type: "start", thinking });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    const instruction = prompt.trim();
    if (!instruction || !ready) return;
    const id = nextId.current++;
    startedAt.current = Date.now();
    setElapsedMs(0);
    setStatus("running");
    setLiveAnswer("");
    setReasoning("");
    setPrompt("");
    setPending((current) => new Set(current).add(id));
    setTranscript((current) => [...current, { id, role: "user", text: instruction }]);
    workerRef.current?.postMessage({ type: "prompt", id, prompt: instruction });
  }

  return (
    <main className="shell">
      <header className="hero">
        <div>
          <div className="eyebrow"><span className="live-dot" /> NANOCODEX / WASM LAB</div>
          <h1>The agent loop,<br /><em>inside your browser.</em></h1>
          <p className="lede">
            React drives a dedicated Worker. Rust/WASM owns the persistent Responses session,
            typed history, and tool loop. A Cloudflare Worker holds the API secret at the edge.
          </p>
        </div>
        <div className="architecture" aria-label="Runtime architecture">
          <RuntimeStep number="01" title="React" note="controls + live trace" />
          <RuntimeStep number="02" title="Web Worker" note="isolated host boundary" />
          <RuntimeStep number="03" title="Rust / WASM" note="session + model loop" active />
          <RuntimeStep number="04" title="CF Worker API" note="secret-bound upgrade" />
          <RuntimeStep number="05" title="OpenAI Responses" note="default WebSocket API" />
        </div>
      </header>

      <section className="control-panel">
        <form className="connection-controls" onSubmit={start}>
          <label className="endpoint-field">
            <span>OpenAI Responses WebSocket</span>
            <input
              value="wss://api.openai.com/v1/responses"
              readOnly
              aria-label="Default OpenAI Responses WebSocket endpoint"
            />
          </label>
          <label>
            <span>Reasoning</span>
            <select value={thinking} onChange={(event) => setThinking(event.target.value as Thinking)}>
              <option value="low">low</option>
              <option value="medium">medium</option>
              <option value="high">high</option>
              <option value="xhigh">xhigh</option>
            </select>
          </label>
          <button className="connect" type="submit">{ready ? "Reset session" : "Start agent"}</button>
        </form>

        <div className="panel-divider" />

        <form onSubmit={submit}>
          <div className="prompt-heading">
            <label htmlFor="prompt">Next prompt</label>
            <span>{prompt.length.toLocaleString()} chars · follow-on context retained</span>
          </div>
          <textarea
            id="prompt"
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            disabled={!ready}
            spellCheck="false"
            placeholder="Start the agent, then send a prompt."
          />
          <div className="run-row">
            <p><code>OPENAI_API_KEY</code> is a Worker secret binding; no credential enters this page.</p>
            <div className="run-controls">
              <span className={`status status-${status}`}>{status}</span>
              <button className="run" type="submit" disabled={!ready || !prompt.trim()}>
                Queue turn <span>↗</span>
              </button>
            </div>
          </div>
        </form>
      </section>

      <section className="metrics" aria-label="Session metrics">
        <Metric label="Wall time" value={formatDuration(elapsedMs)} />
        <Metric label="Model calls" value={formatNumber(stats?.model_calls ?? count(events, "model.call.started"))} />
        <Metric label="Tool calls" value={formatNumber(stats?.tool_calls ?? tools.length)} />
        <Metric label="Total tokens" value={formatNumber(usage?.total_tokens)} />
        <Metric label="WS connects" value={formatNumber(stats?.connection_attempts ?? count(events, "model.connection.completed"))} />
      </section>

      <section className="workspace-grid">
        <article className="card answer-card">
          <CardHeader index="A" title="Live session" meta={`${transcript.length} messages`} />
          <div className="transcript">
            {!transcript.length && !liveAnswer && <Empty text="Your persistent conversation will appear here." />}
            {transcript.map((item) => <Message key={`${item.role}-${item.id}`} item={item} />)}
            {status === "running" && liveAnswer && (
              <div className="message assistant streaming"><span>assistant / live</span><p>{liveAnswer}<i className="cursor" /></p></div>
            )}
            {pending.size > 0 && !liveAnswer && <div className="thinking-line"><span /> Rust/WASM is working…</div>}
          </div>
        </article>

        <article className="card trace-card">
          <CardHeader index="B" title="Execution trace" meta={`${events.length} events`} />
          <div className="trace">
            {!events.length && <Empty text="No runtime events yet." />}
            {tools.map((tool, index) => <ToolEntry key={tool.call_id ?? index} tool={tool} index={index} />)}
            {events.filter(isLifecycleEvent).map((event) => <LifecycleEntry key={`${event.seq}-${event.type}`} event={event} />)}
          </div>
        </article>

        <article className="card reasoning-card">
          <CardHeader index="C" title="Reasoning summary" meta={reasoning ? "streamed" : "optional"} />
          <div className={`reasoning ${reasoning ? "" : "empty"}`}>
            {reasoning || "API-visible reasoning summaries for the current turn will appear here."}
          </div>
        </article>

        <article className="card json-card">
          <CardHeader index="D" title="Raw events" meta={`${events.length} records`} />
          <pre>{events.length ? events.map((event) => JSON.stringify(event)).join("\n") : "// Exact typed event JSON will appear here."}</pre>
        </article>
      </section>
    </main>
  );
}

function RuntimeStep({ number, title, note, active = false }: { number: string; title: string; note: string; active?: boolean }) {
  return <div className={`runtime-step ${active ? "active" : ""}`}><span>{number}</span><strong>{title}</strong><small>{note}</small></div>;
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div className="metric"><span>{label}</span><strong>{value}</strong></div>;
}

function CardHeader({ index, title, meta }: { index: string; title: string; meta: string }) {
  return <header className="card-header"><span>{index}</span><h2>{title}</h2><small>{meta}</small></header>;
}

function Empty({ text }: { text: string }) {
  return <div className="empty trace-empty">{text}</div>;
}

function Message({ item }: { item: TranscriptItem }) {
  return <div className={`message ${item.role}`}><span>{item.role}</span><p>{item.text}</p></div>;
}

function ToolEntry({ tool, index }: { tool: ToolTrace; index: number }) {
  return (
    <details className="tool-entry" open>
      <summary>
        <span className="trace-index">{String(index + 1).padStart(2, "0")}</span>
        <strong>{tool.tool ?? "exec"}</strong>
        <span className={`tool-state ${tool.status ?? "running"}`}>{tool.status ?? "running"}</span>
        <small>{tool.duration_ns ? formatNs(tool.duration_ns) : ""}</small>
      </summary>
      <div className="tool-body">
        <div><span>arguments</span><pre>{formatJson(tool.arguments)}</pre></div>
        {tool.result !== undefined && <div><span>result</span><pre>{formatJson(tool.result)}</pre></div>}
      </div>
    </details>
  );
}

function LifecycleEntry({ event }: { event: AgentEvent }) {
  return (
    <div className="lifecycle-entry">
      <span className="trace-index">{String(event.seq).padStart(2, "0")}</span>
      <strong>{event.type}</strong>
      <small>{payloadNumber(event.payload, "duration_ns") ? formatNs(payloadNumber(event.payload, "duration_ns")!) : ""}</small>
    </div>
  );
}

function toolTimeline(events: AgentEvent[]): ToolTrace[] {
  const byId = new Map<string, ToolTrace>();
  for (const event of events) {
    const callId = payloadString(event.payload, "call_id");
    if (!callId) continue;
    if (event.type === "tool.call") {
      byId.set(callId, { ...event.payload, call_id: callId, status: "running" });
    } else if (event.type === "tool.result") {
      byId.set(callId, { ...byId.get(callId), ...event.payload, call_id: callId });
    }
  }
  return [...byId.values()];
}

function isLifecycleEvent(event: AgentEvent): boolean {
  return [
    "run.started",
    "model.connection.completed",
    "model.warmup.completed",
    "model.call.completed",
    "model.attempt.retrying",
    "run.completed",
    "run.failed",
  ].includes(event.type);
}

function payloadText(event: AgentEvent): string {
  return payloadString(event.payload, "text") ?? "";
}

function payloadString(payload: JsonObject, key: string): string | undefined {
  return typeof payload[key] === "string" ? payload[key] : undefined;
}

function payloadNumber(payload: JsonObject, key: string): number | undefined {
  return typeof payload[key] === "number" ? payload[key] : undefined;
}

function asObject(value: unknown): JsonObject | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as JsonObject : undefined;
}

function count(events: AgentEvent[], type: string): number {
  return events.filter((event) => event.type === type).length;
}

function formatDuration(milliseconds: number): string {
  if (milliseconds < 1_000) return `${Math.round(milliseconds)} ms`;
  return `${(milliseconds / 1_000).toFixed(milliseconds < 10_000 ? 2 : 1)} s`;
}

function formatNs(nanoseconds: number): string {
  return formatDuration(nanoseconds / 1_000_000);
}

function formatNumber(value: unknown): string {
  return typeof value === "number" ? value.toLocaleString() : "—";
}

function formatJson(value: unknown): string {
  if (typeof value === "string") return value;
  return value === undefined ? "" : JSON.stringify(value, null, 2);
}

function without(values: Set<number>, value: number): Set<number> {
  const next = new Set(values);
  next.delete(value);
  return next;
}
