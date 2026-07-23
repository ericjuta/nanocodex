import { subscribeAgentEvents } from "../internal.mjs";

export function watch(agent, options = {}) {
  const listeners = new Set();
  const iterators = new Set();
  let unsubscribe;
  let closed = false;

  const emit = (event) => {
    for (const listener of listeners) listener(event);
    for (const iterator of iterators) iterator.push(event);
  };

  const start = () => {
    if (closed || unsubscribe) return;
    unsubscribe = subscribeAgentEvents(agent, emit, options);
  };

  const watcher = {
    onEvent(listener) {
      if (typeof listener !== "function") throw new TypeError("events.watch.onEvent requires a listener");
      if (closed) return () => {};
      listeners.add(listener);
      start();
      return () => listeners.delete(listener);
    },
    off() {
      if (closed) return;
      closed = true;
      unsubscribe?.();
      unsubscribe = undefined;
      listeners.clear();
      for (const iterator of [...iterators]) iterator.end();
      iterators.clear();
    },
    [Symbol.asyncIterator]() {
      if (closed) return emptyIterator();
      const iterator = eventIterator(() => iterators.delete(iterator));
      iterators.add(iterator);
      start();
      return iterator;
    },
  };
  return Object.freeze(watcher);
}

function eventIterator(onEnd) {
  const queue = [];
  let pending;
  let done = false;

  const iterator = {
    push(event) {
      if (done) return;
      if (pending) {
        const resolve = pending;
        pending = undefined;
        resolve({ done: false, value: event });
      } else {
        queue.push(event);
      }
    },
    end() {
      if (done) return;
      done = true;
      onEnd();
      pending?.({ done: true, value: undefined });
      pending = undefined;
      queue.length = 0;
    },
    next() {
      if (queue.length) return Promise.resolve({ done: false, value: queue.shift() });
      if (done) return Promise.resolve({ done: true, value: undefined });
      return new Promise((resolve) => { pending = resolve; });
    },
    return() {
      iterator.end();
      return Promise.resolve({ done: true, value: undefined });
    },
    [Symbol.asyncIterator]() { return this; },
  };
  return iterator;
}

function emptyIterator() {
  return {
    next: () => Promise.resolve({ done: true, value: undefined }),
    return: () => Promise.resolve({ done: true, value: undefined }),
    [Symbol.asyncIterator]() { return this; },
  };
}
