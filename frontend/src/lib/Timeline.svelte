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
    padding: 0.5rem 0.75rem;
    border-top: 1px solid var(--border, #333);
  }

  .empty {
    color: var(--muted-fg, #888);
    font-size: 0.85rem;
    padding: 0.25rem 0;
  }

  .strip {
    display: flex;
    gap: 0.4rem;
    overflow-x: auto;
    padding-bottom: 0.2rem;
  }

  .iteration {
    flex: 0 0 auto;
    font: inherit;
    padding: 0.3rem 0.7rem;
    border-radius: 0.4rem;
    border: 1px solid var(--border, #444);
    background: var(--button-bg, #2b2d31);
    color: inherit;
    cursor: pointer;
    font-size: 0.85rem;
  }

  .iteration.current {
    background: var(--accent, #2b5fd9);
    color: #fff;
    border-color: transparent;
  }

  .viewing-indicator {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    font-size: 0.8rem;
    color: var(--muted-fg, #aaa);
  }

  .back-to-latest {
    font: inherit;
    padding: 0.2rem 0.6rem;
    border-radius: 0.3rem;
    border: 1px solid var(--border, #444);
    background: var(--button-bg, #2b2d31);
    color: inherit;
    cursor: pointer;
    font-size: 0.75rem;
  }
</style>
