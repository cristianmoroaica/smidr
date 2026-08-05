<script lang="ts" module>
  type RefEntry = { slug: string; name: string; category: string };

  let cache: RefEntry[] | null = null;
  let inflight: Promise<RefEntry[]> | null = null;

  async function loadRefs(): Promise<RefEntry[]> {
    if (cache) return cache;
    if (inflight) return inflight;
    inflight = (async () => {
      try {
        const res = await fetch('/api/refs');
        if (!res.ok) return [];
        const data = (await res.json()) as unknown;
        if (!Array.isArray(data)) return [];
        return data.filter(
          (e): e is RefEntry =>
            typeof e === 'object' &&
            e !== null &&
            typeof (e as RefEntry).slug === 'string' &&
            typeof (e as RefEntry).name === 'string' &&
            typeof (e as RefEntry).category === 'string'
        );
      } catch {
        return [];
      }
    })();
    cache = await inflight;
    inflight = null;
    return cache;
  }
</script>

<script lang="ts">
  let {
    query,
    onChoose,
    onDismiss,
    highlightedIndex = $bindable(0)
  }: {
    query: string;
    onChoose: (slug: string) => void;
    onDismiss: () => void;
    highlightedIndex?: number;
  } = $props();

  let entries = $state<RefEntry[]>([]);

  $effect(() => {
    loadRefs().then((r) => (entries = r));
  });

  let filtered = $derived(
    entries
      .filter((e) => {
        const q = query.toLowerCase();
        return e.slug.toLowerCase().includes(q) || e.name.toLowerCase().includes(q);
      })
      .slice(0, 8)
  );

  $effect(() => {
    if (highlightedIndex >= filtered.length) {
      highlightedIndex = filtered.length > 0 ? filtered.length - 1 : 0;
    }
  });

  export function handleKeydown(e: KeyboardEvent): boolean {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (filtered.length > 0) {
        highlightedIndex = (highlightedIndex + 1) % filtered.length;
      }
      return true;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (filtered.length > 0) {
        highlightedIndex = (highlightedIndex - 1 + filtered.length) % filtered.length;
      }
      return true;
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      if (filtered.length > 0) {
        e.preventDefault();
        onChoose(filtered[highlightedIndex].slug);
      }
      return true;
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      onDismiss();
      return true;
    }
    return false;
  }
</script>

<div class="ref-picker">
  {#if filtered.length === 0}
    <div class="item muted">no matching references</div>
  {:else}
    {#each filtered as entry, i}
      <button
        type="button"
        class="item"
        class:active={i === highlightedIndex}
        onmousedown={(e) => {
          e.preventDefault();
          onChoose(entry.slug);
        }}
      >
        <span class="name">{entry.name}</span>
        <span class="meta">{entry.slug} · {entry.category}</span>
      </button>
    {/each}
  {/if}
</div>

<style>
  .ref-picker {
    display: flex;
    flex-direction: column;
    background: var(--tool-call-bg, #1b1c20);
    border: 1px solid var(--tool-call-border, #35363c);
    border-radius: 0.4rem;
    overflow: hidden;
    max-height: 12rem;
    overflow-y: auto;
  }

  .item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
    padding: 0.35rem 0.6rem;
    background: transparent;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    width: 100%;
  }

  .item.active,
  .item:hover {
    background: var(--button-bg, #2b2d31);
  }

  .item.muted {
    opacity: 0.6;
    cursor: default;
  }

  .name {
    font-size: 0.85rem;
  }

  .meta {
    font-size: 0.75rem;
    opacity: 0.6;
    white-space: nowrap;
  }
</style>
