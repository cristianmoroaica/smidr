<script lang="ts">
  let {
    iterations,
    viewing,
    onSelect
  }: {
    iterations: number[];
    viewing: number | null;
    onSelect: (n: number | null) => void;
  } = $props();

  let latest = $derived(iterations.length > 0 ? iterations[iterations.length - 1] : null);
  let current = $derived(viewing !== null ? viewing : latest);
  // Historical mode only when actually pinned to an older iteration.
  let historical = $derived(viewing !== null && viewing !== latest);
</script>

<div class="timeline">
  {#if iterations.length === 0}
    <div class="empty">No iterations yet</div>
  {:else}
    <div class="strip">
      {#each iterations as n}
        <button
          class="iteration"
          class:current={n === current}
          onclick={() => onSelect(n === latest ? null : n)}
        >
          v{n}
        </button>
      {/each}
    </div>
    {#if historical}
      <div class="viewing-indicator">
        <span>viewing v{viewing}</span>
        <button class="back-to-latest" onclick={() => onSelect(null)}>Back to latest</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .timeline {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.5rem 0.8rem;
    background: var(--bg-app, #111318);
    border-top: 1px solid var(--border, #2e333d);
  }

  .empty {
    color: var(--text-muted, #6b7280);
    font-size: 0.8rem;
    padding: 0.25rem 0;
  }

  .strip {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    overflow-x: auto;
    padding-bottom: 0.15rem;
  }

  .iteration {
    flex: 0 0 auto;
    font-size: 0.78rem;
    font-weight: 600;
    font-family: var(--font-mono, monospace);
    padding: 0.3rem 0.75rem;
    min-width: 2.75rem;
    text-align: center;
    border-radius: var(--radius-pill, 999px);
    border: 1px solid var(--border-strong, #3d434f);
    background: var(--bg-raised, #22262f);
    color: var(--text-secondary, #9aa3b2);
    cursor: pointer;
    transition:
      background 120ms,
      color 120ms,
      border-color 120ms;
  }

  .iteration:hover {
    background: var(--bg-hover, #2a2f3a);
    color: var(--text, #e6e9ef);
    border-color: var(--accent-border, rgba(79, 143, 247, 0.45));
  }

  .iteration:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .iteration.current {
    background: var(--accent, #4f8ff7);
    color: var(--accent-fg, #0b1220);
    border-color: transparent;
  }

  .viewing-indicator {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    font-size: 0.75rem;
    color: var(--warn, #d29922);
    background: rgba(210, 153, 34, 0.12);
    border: 1px solid rgba(210, 153, 34, 0.4);
    border-radius: var(--radius-sm, 6px);
    padding: 0.3rem 0.6rem;
  }

  .back-to-latest {
    font: inherit;
    font-family: inherit;
    font-size: 0.72rem;
    padding: 0.2rem 0.6rem;
    border-radius: var(--radius-sm, 6px);
    border: 1px solid rgba(210, 153, 34, 0.5);
    background: transparent;
    color: var(--warn, #d29922);
    cursor: pointer;
    margin-left: auto;
  }

  .back-to-latest:hover {
    background: rgba(210, 153, 34, 0.18);
  }
</style>
