import {
  DEFAULT_THEMES,
  parsePatchFiles,
  type CodeViewItem,
  type CodeViewOptions,
} from "@pierre/diffs";
import {
  CodeView,
  type CodeViewHandle,
  useStableCallback,
} from "@pierre/diffs/react";
import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  usePierreMainHighlighter,
  usePierreRenderer,
} from "./PierreWorkerProvider";
import { COMMIT_CODE_VIEW_CUSTOM_CSS, CODE_VIEW_LAYOUT } from "./pierreCodeView";
import type { HarnessCommit, Theme } from "./Xedoc";

const CODE_VIEW_BATCH_COUNT = 25;
const CODE_VIEW_BATCH_COUNT_MAX = 96;
const CODE_VIEW_FILE_TREE_ITEM_HEIGHT = 24;
const STREAM_INITIAL_PUBLISH_INTERVAL_MS = 500;
const STREAM_WORK_BUDGET_MS = 8;
const PATCH_FETCH_CONCURRENCY = 6;

const dateFormatter = new Intl.DateTimeFormat("en", {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

type CommitCodeStreamProps = {
  commits: HarnessCommit[];
  theme: Theme;
};

type CommitAnnotation = undefined;
type CommitStreamItem = CodeViewItem<CommitAnnotation>;

export type CommitCodeStreamHandle = {
  focus(): void;
  scrollToCommit(index: number): void;
};

function commitItemId(commit: HarnessCommit) {
  return `commit:${commit.hash}`;
}

function createCommitItem(commit: HarnessCommit): CommitStreamItem {
  return {
    id: commitItemId(commit),
    type: "file",
    file: {
      name: "",
      contents: "",
      lang: "markdown",
      cacheKey: `${commit.hash}:message`,
    },
  };
}

function yieldToBrowser(): Promise<void> {
  return new Promise((resolve) => {
    let didResolve = false;
    const resolveOnce = () => {
      if (didResolve) return;
      didResolve = true;
      window.clearTimeout(timeout);
      resolve();
    };
    const timeout = window.setTimeout(resolveOnce, 50);
    window.requestAnimationFrame(resolveOnce);
  });
}

function getInitialBatchSize() {
  const viewportHeight = window.visualViewport?.height ?? window.innerHeight;
  if (!Number.isFinite(viewportHeight) || viewportHeight <= 0) {
    return CODE_VIEW_BATCH_COUNT;
  }
  return Math.min(
    CODE_VIEW_BATCH_COUNT_MAX,
    Math.max(
      CODE_VIEW_BATCH_COUNT,
      Math.ceil(viewportHeight / CODE_VIEW_FILE_TREE_ITEM_HEIGHT),
    ),
  );
}

export const CommitCodeStream = forwardRef<
  CommitCodeStreamHandle,
  CommitCodeStreamProps
>(function CommitCodeStream({ commits, theme }, forwardedRef) {
  const renderer = usePierreRenderer();
  const mainHighlighterReady = usePierreMainHighlighter();
  const viewerRef = useRef<CodeViewHandle<CommitAnnotation> | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const commitIndexByItemIdRef = useRef(new Map<string, number>());
  const loadedCommitIdsRef = useRef<Array<string | undefined>>([]);
  const pendingJumpRef = useRef<number | null>(null);
  const [viewerGeneration, setViewerGeneration] = useState(0);
  const [initialItems, setInitialItems] = useState<CommitStreamItem[]>([]);
  const renderReady =
    renderer.ready && (!renderer.disableWorkerPool || mainHighlighterReady);

  const scrollToCommit = useCallback((index: number) => {
    const id = loadedCommitIdsRef.current[index];
    const viewer = viewerRef.current;
    if (id == null || viewer == null) {
      pendingJumpRef.current = index;
      return;
    }
    pendingJumpRef.current = null;
    viewer.scrollTo({
      type: "item",
      id,
      align: "start",
      behavior: "smooth",
    });
  }, []);

  useImperativeHandle(
    forwardedRef,
    () => ({
      focus() {
        containerRef.current?.focus({ preventScroll: true });
      },
      scrollToCommit,
    }),
    [scrollToCommit],
  );

  const options = useMemo<CodeViewOptions<CommitAnnotation>>(
    () => ({
      layout: CODE_VIEW_LAYOUT,
      theme: DEFAULT_THEMES,
      themeType: theme,
      diffStyle: "unified",
      diffIndicators: "bars",
      overflow: "scroll",
      lineHoverHighlight: "number",
      enableLineSelection: true,
      stickyHeaders: true,
      itemMetrics: {
        lineHeight: 18,
        diffHeaderHeight: 44,
      },
      unsafeCSS: COMMIT_CODE_VIEW_CUSTOM_CSS,
    }),
    [theme],
  );

  const renderStreamHeader = useStableCallback((item: CommitStreamItem) => {
    if (item.type === "diff") {
      const additions = item.fileDiff.hunks.reduce(
        (total, hunk) => total + hunk.additionLines,
        0,
      );
      const deletions = item.fileDiff.hunks.reduce(
        (total, hunk) => total + hunk.deletionLines,
        0,
      );
      const path = item.fileDiff.prevName
        ? `${item.fileDiff.prevName} → ${item.fileDiff.name}`
        : item.fileDiff.name;
      return (
        <div className="commit-file-header">
          <span
            className={`commit-file-status is-${item.fileDiff.type}`}
            aria-hidden="true"
          />
          <span className="commit-file-path">{path}</span>
          <span className="commit-file-stats">
            {deletions > 0 ? <span className="deletions">−{deletions}</span> : null}
            {additions > 0 ? <span className="additions">+{additions}</span> : null}
          </span>
        </div>
      );
    }

    const commitIndex = commitIndexByItemIdRef.current.get(item.id);
    if (commitIndex == null) return null;
    const commit = commits[commitIndex];
    return (
      <article
        className="commit-section-header"
        aria-labelledby={`commit-title-${commit.shortHash}`}
      >
        <h2 id={`commit-title-${commit.shortHash}`}>{commit.subject}</h2>
        <div className="commit-code-metadata">
          <span className="commit-section-hash">Commit {commit.shortHash}</span>
          <span>{commit.author}</span>
          <span>{dateFormatter.format(new Date(commit.authoredAt))}</span>
          <span>
            {commit.stats.files} file{commit.stats.files === 1 ? "" : "s"}
          </span>
          <span className="additions">+{commit.stats.additions}</span>
          <span className="deletions">−{commit.stats.deletions}</span>
        </div>
      </article>
    );
  });

  useEffect(() => {
    if (!renderReady) return;

    const controller = new AbortController();
    let current = true;
    commitIndexByItemIdRef.current = new Map();
    loadedCommitIdsRef.current = [];
    pendingJumpRef.current = null;
    setViewerGeneration((generation) => generation + 1);
    setInitialItems([]);

    async function loadCommits() {
      // Let the keyed CodeView remount before appending the first batch. This
      // also makes repository re-syncs and Fast Refresh restart cleanly.
      await yieldToBrowser();
      if (!current) return;

      let pendingItems: CommitStreamItem[] = [];
      let pendingCommitIndexes: number[] = [];
      let lastWorkYieldTime = performance.now();
      let lastPublishTime = lastWorkYieldTime;
      let hasPublishedItems = false;
      const initialBatchSize = getInitialBatchSize();

      const publish = async () => {
        if (!current || pendingItems.length === 0) return;
        const publishedCommitIndexes = pendingCommitIndexes;
        const publishedItems = pendingItems;
        if (!hasPublishedItems) {
          setInitialItems(publishedItems);
        } else if (viewerRef.current != null) {
          viewerRef.current.addItems(publishedItems);
        } else {
          setInitialItems((currentItems) => [...currentItems, ...publishedItems]);
        }
        for (const commitIndex of publishedCommitIndexes) {
          loadedCommitIdsRef.current[commitIndex] = commitItemId(commits[commitIndex]);
        }
        pendingItems = [];
        pendingCommitIndexes = [];
        hasPublishedItems = true;
        lastPublishTime = performance.now();
        const publishedCommitCount = (publishedCommitIndexes.at(-1) ?? -1) + 1;
        await yieldToBrowser();
        lastWorkYieldTime = performance.now();

        const pendingJump = pendingJumpRef.current;
        if (pendingJump != null && pendingJump < publishedCommitCount) {
          scrollToCommit(pendingJump);
        }
      };

      for (let start = 0; start < commits.length; start += PATCH_FETCH_CONCURRENCY) {
        const group = commits.slice(start, start + PATCH_FETCH_CONCURRENCY);
        const patches = await Promise.all(
          group.map(async (commit) => {
            try {
              const response = await fetch(commit.patchUrl, {
                cache: "force-cache",
                signal: controller.signal,
              });
              if (!response.ok) throw new Error(`Patch request failed: ${response.status}`);
              return await response.text();
            } catch (error) {
              if (controller.signal.aborted) throw error;
              return null;
            }
          }),
        );
        if (!current) return;

        for (let offset = 0; offset < group.length; offset++) {
          const commitIndex = start + offset;
          const commit = group[offset];
          const boundary = createCommitItem(commit);
          const commitItems: CommitStreamItem[] = [boundary];
          commitIndexByItemIdRef.current.set(boundary.id, commitIndex);

          const patch = patches[offset];
          if (patch != null) {
            const fileDiffs = parsePatchFiles(patch, commit.hash).flatMap(
              (parsedPatch) => parsedPatch.files,
            );
            for (let fileIndex = 0; fileIndex < fileDiffs.length; fileIndex++) {
              const item: CommitStreamItem = {
                id: `${commit.hash}:${fileIndex}:${fileDiffs[fileIndex].name}`,
                type: "diff",
                fileDiff: fileDiffs[fileIndex],
              };
              commitItems.push(item);
            }
          }

          const batchSize = hasPublishedItems
            ? CODE_VIEW_BATCH_COUNT
            : initialBatchSize;
          if (pendingItems.length > 0 && pendingItems.length + commitItems.length > batchSize) {
            await publish();
          }
          pendingItems.push(...commitItems);
          pendingCommitIndexes.push(commitIndex);

          const nextBatchSize = hasPublishedItems
            ? CODE_VIEW_BATCH_COUNT
            : initialBatchSize;
          if (pendingItems.length >= nextBatchSize) {
            await publish();
          } else if (performance.now() - lastWorkYieldTime >= STREAM_WORK_BUDGET_MS) {
            const deferInitialBatch =
              !hasPublishedItems &&
              pendingItems.length < initialBatchSize &&
              performance.now() - lastPublishTime < STREAM_INITIAL_PUBLISH_INTERVAL_MS;
            if (deferInitialBatch) {
              await yieldToBrowser();
              lastWorkYieldTime = performance.now();
            } else {
              await publish();
            }
          }
        }
      }

      await publish();
    }

    void loadCommits().catch((error) => {
      if (!controller.signal.aborted) console.warn("Failed to load commit stream", error);
    });

    return () => {
      current = false;
      controller.abort();
    };
  }, [commits, renderReady, renderer.disableWorkerPool, scrollToCommit]);

  if (!renderReady || initialItems.length === 0) {
    return null;
  }

  return (
    <CodeView
      key={`${renderer.disableWorkerPool ? "main" : "workers"}:${viewerGeneration}`}
      ref={viewerRef}
      containerRef={containerRef}
      initialItems={initialItems}
      className="commit-stream code-view cv-scrollbar"
      disableWorkerPool={renderer.disableWorkerPool}
      options={options}
      renderCustomHeader={renderStreamHeader}
    />
  );
});
