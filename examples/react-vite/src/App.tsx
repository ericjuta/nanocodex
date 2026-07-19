import { FormEvent, useEffect, useRef, useState } from "react";

type Thinking = "low" | "medium" | "high" | "xhigh";
type TranscriptItem = {
  id: number;
  role: "user" | "assistant" | "error";
  text: string;
};

type WorkerMessage =
  | { type: "ready" }
  | { type: "event"; event: unknown }
  | { type: "result"; id: number; message: string }
  | { type: "error"; id?: number; message: string };

export function App() {
  const workerRef = useRef<Worker | null>(null);
  const nextId = useRef(1);
  const [endpoint, setEndpoint] = useState("");
  const [thinking, setThinking] = useState<Thinking>("medium");
  const [ready, setReady] = useState(false);
  const [prompt, setPrompt] = useState("");
  const [pending, setPending] = useState<Set<number>>(() => new Set());
  const [transcript, setTranscript] = useState<TranscriptItem[]>([]);
  const [events, setEvents] = useState<unknown[]>([]);

  useEffect(() => {
    const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module" });
    workerRef.current = worker;
    worker.onmessage = ({ data }: MessageEvent<WorkerMessage>) => {
      if (data.type === "ready") {
        setReady(true);
      } else if (data.type === "event") {
        setEvents((current) => [...current.slice(-199), data.event]);
      } else if (data.type === "result") {
        setPending((current) => without(current, data.id));
        setTranscript((current) => [
          ...current,
          { id: data.id, role: "assistant", text: data.message },
        ]);
      } else {
        if (data.id !== undefined) setPending((current) => without(current, data.id!));
        setTranscript((current) => [
          ...current,
          { id: data.id ?? nextId.current++, role: "error", text: data.message },
        ]);
      }
    };
    return () => worker.terminate();
  }, []);

  function start(event: FormEvent) {
    event.preventDefault();
    const websocketUrl = endpoint.trim();
    if (!websocketUrl) return;
    setReady(false);
    setEvents([]);
    setTranscript([]);
    workerRef.current?.postMessage({ type: "start", websocketUrl, thinking });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    const instruction = prompt.trim();
    if (!instruction || !ready) return;
    const id = nextId.current++;
    setPrompt("");
    setPending((current) => new Set(current).add(id));
    setTranscript((current) => [...current, { id, role: "user", text: instruction }]);
    workerRef.current?.postMessage({ type: "prompt", id, prompt: instruction });
  }

  return (
    <main>
      <header>
        <p className="eyebrow">Embedded agents SDK</p>
        <h1>Nanocodex in React</h1>
        <p>The Rust agent and its persistent Responses session run inside a browser Worker.</p>
      </header>

      <form className="connection" onSubmit={start}>
        <label>
          Authenticated WebSocket URL
          <input
            type="url"
            value={endpoint}
            onChange={(event) => setEndpoint(event.target.value)}
            placeholder="wss://your-authorized-endpoint.example/responses"
            required
          />
        </label>
        <label>
          Thinking
          <select value={thinking} onChange={(event) => setThinking(event.target.value as Thinking)}>
            <option value="low">Low</option>
            <option value="medium">Medium</option>
            <option value="high">High</option>
            <option value="xhigh">Extra high</option>
          </select>
        </label>
        <button type="submit">{ready ? "Restart agent" : "Start agent"}</button>
      </form>

      <section className="status" data-ready={ready}>
        <span /> {ready ? "Agent ready—follow-on context is retained" : "Waiting for an endpoint"}
      </section>

      <section className="transcript" aria-live="polite">
        {transcript.length === 0 && (
          <p className="empty">Start the agent, then ask it to call <code>tools.browserInfo()</code>.</p>
        )}
        {transcript.map((item) => (
          <article key={`${item.role}-${item.id}`} className={item.role}>
            <strong>{item.role}</strong>
            <p>{item.text}</p>
          </article>
        ))}
        {pending.size > 0 && <p className="pending">Running {pending.size} turn{pending.size === 1 ? "" : "s"}…</p>}
      </section>

      <form className="composer" onSubmit={submit}>
        <textarea
          value={prompt}
          onChange={(event) => setPrompt(event.target.value)}
          placeholder="Ask a prompt; the next one automatically continues the same session."
          disabled={!ready}
          rows={3}
        />
        <button type="submit" disabled={!ready || !prompt.trim()}>Send</button>
      </form>

      <details>
        <summary>Ordered agent events ({events.length})</summary>
        <pre>{events.map((event) => JSON.stringify(event)).join("\n")}</pre>
      </details>
    </main>
  );
}

function without(values: Set<number>, value: number): Set<number> {
  const next = new Set(values);
  next.delete(value);
  return next;
}
