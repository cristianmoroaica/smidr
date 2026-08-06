<script lang="ts">
  type EngineEntry = {
    id: string;
    name: string;
    available: boolean;
    models: string[] | null;
  };

  let {
    engine,
    onSelect
  }: {
    engine: string;
    onSelect: (engine: string) => void;
  } = $props();

  let entries = $state<EngineEntry[]>([]);
  let loadFailed = $state(false);
  let loaded = $state(false);
  let open = $state(false);
  let expandedId = $state<string | null>(null);
  let modelInputs = $state<Record<string, string>>({});

  let root: HTMLDivElement | undefined;
  let triggerEl: HTMLButtonElement | undefined = $state();

  $effect(() => {
    if (loaded) return;
    loaded = true;
    (async () => {
      try {
        const res = await fetch('/api/engines');
        if (!res.ok) throw new Error(`status ${res.status}`);
        const data = (await res.json()) as unknown;
        if (!Array.isArray(data)) throw new Error('unexpected shape');
        entries = data.filter(
          (e): e is EngineEntry =>
            typeof e === 'object' &&
            e !== null &&
            typeof (e as EngineEntry).id === 'string' &&
            typeof (e as EngineEntry).name === 'string' &&
            typeof (e as EngineEntry).available === 'boolean' &&
            ((e as EngineEntry).models === null || Array.isArray((e as EngineEntry).models))
        );
      } catch (err) {
        loadFailed = true;
        console.warn('failed to load /api/engines', err);
      }
    })();
  });

  function label(value: string): string {
    if (value === 'claude') return 'Claude';
    const idx = value.indexOf(':');
    if (idx === -1) return value;
    return `${value.slice(0, idx)} · ${value.slice(idx + 1)}`;
  }

  // Endpoint name of the currently-selected engine, or null for claude.
  // Same first-colon split the server uses (engine_config::split_engine_id).
  function currentEndpointId(): string | null {
    if (engine === 'claude') return null;
    const idx = engine.indexOf(':');
    return idx === -1 ? null : engine.slice(0, idx);
  }

  function openPanel() {
    open = true;
    // Reveal the selected model without a second click.
    expandedId = currentEndpointId();
  }

  function closePanel(returnFocus: boolean) {
    open = false;
    expandedId = null;
    if (returnFocus) triggerEl?.focus();
  }

  function selectEntry(id: string) {
    onSelect(id);
    closePanel(true);
  }

  function selectModel(id: string, model: string) {
    onSelect(`${id}:${model}`);
    closePanel(true);
  }

  function toggleExpand(id: string) {
    expandedId = expandedId === id ? null : id;
  }

  function submitModelInput(id: string) {
    const value = (modelInputs[id] ?? '').trim();
    if (!value) return;
    selectModel(id, value);
  }

  function handleEntryClick(e: EngineEntry) {
    if (!e.available) return;
    if (e.id === 'claude') {
      selectEntry('claude');
      return;
    }
    toggleExpand(e.id);
  }

  function handleWindowClick(e: MouseEvent) {
    if (!root) return;
    if (!root.contains(e.target as Node)) closePanel(false);
  }

  function handleWindowKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) closePanel(true);
  }
</script>

<svelte:window onclick={handleWindowClick} onkeydown={handleWindowKeydown} />

<div class="engine-picker" bind:this={root}>
  {#if loadFailed}
    <button type="button" class="trigger disabled" disabled title={engine}>
      {label(engine)}
    </button>
  {:else}
    <button
      type="button"
      class="trigger"
      title={engine}
      bind:this={triggerEl}
      onclick={() => (open ? closePanel(false) : openPanel())}
      aria-haspopup="menu"
      aria-expanded={open}
    >
      {label(engine)}
    </button>
    {#if open}
      <div class="panel" role="menu">
        {#if entries.length === 0}
          <div class="option placeholder">Loading engines…</div>
        {/if}
        {#each entries as e (e.id)}
          <div class="group">
            <button
              type="button"
              class="option"
              role="menuitem"
              class:selected={e.id === 'claude' ? engine === 'claude' : e.id === currentEndpointId()}
              class:disabled-option={!e.available}
              disabled={!e.available}
              onclick={() => handleEntryClick(e)}
            >
              <span>{e.name}</span>
              {#if !e.available}
                <span class="hint">not found</span>
              {/if}
            </button>
            {#if e.available && e.id !== 'claude' && expandedId === e.id}
              <div class="submenu">
                <!-- An endpoint that reports zero models (e.g. Ollama with
                     nothing pulled) is treated like `models: null`: fall back
                     to the free-text input rather than an empty, dead-end
                     submenu. -->
                {#if e.models && e.models.length > 0}
                  {#each e.models as model (model)}
                    <button
                      type="button"
                      class="model-option"
                      role="menuitem"
                      class:selected={engine === `${e.id}:${model}`}
                      onclick={() => selectModel(e.id, model)}
                    >
                      {model}
                    </button>
                  {/each}
                {:else}
                  <form
                    class="model-input-row"
                    onsubmit={(ev) => {
                      ev.preventDefault();
                      submitModelInput(e.id);
                    }}
                  >
                    <input
                      class="model-input"
                      type="text"
                      placeholder="gpt-oss:120b"
                      bind:value={modelInputs[e.id]}
                    />
                    <button type="submit" class="use-btn">Use</button>
                  </form>
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  {/if}
</div>

<style>
  .engine-picker {
    position: relative;
  }

  .trigger {
    display: inline-block;
    max-width: 11rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: inherit;
    font-size: 0.8rem;
    padding: 0.3rem 0.65rem;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg-raised);
    color: var(--text);
    cursor: pointer;
    transition: background 120ms, border-color 120ms;
  }

  .trigger:hover {
    background: var(--bg-hover);
  }

  .trigger:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .trigger.disabled {
    color: var(--text-muted);
    cursor: default;
  }

  .panel {
    position: absolute;
    top: calc(100% + 0.3rem);
    left: 0;
    z-index: 20;
    min-width: 13rem;
    max-width: 18rem;
    max-height: 20rem;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    background: var(--bg-raised);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-2);
    padding: 0.3rem;
  }

  .group {
    display: flex;
    flex-direction: column;
  }

  .option {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    width: 100%;
    text-align: left;
    font: inherit;
    font-size: 0.82rem;
    padding: 0.4rem 0.55rem;
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text);
    cursor: pointer;
    box-sizing: border-box;
  }

  .option:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .option:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .option.selected {
    background: var(--accent-soft);
    box-shadow: inset 0 0 0 1px var(--accent-border);
  }

  .option.disabled-option,
  .option:disabled {
    color: var(--text-muted);
    cursor: not-allowed;
  }

  .option.placeholder {
    color: var(--text-muted);
    cursor: default;
  }

  .hint {
    font-size: 0.7rem;
    color: var(--text-muted);
  }

  .submenu {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    padding-left: 0.75rem;
    margin: 0.1rem 0 0.2rem;
  }

  .model-option {
    text-align: left;
    font: inherit;
    font-family: var(--font-mono);
    font-size: 0.76rem;
    padding: 0.3rem 0.5rem;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
  }

  .model-option:hover {
    background: var(--bg-hover);
    color: var(--text);
  }

  .model-option:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .model-option.selected {
    background: var(--accent-soft);
    color: var(--text);
  }

  .model-input-row {
    display: flex;
    gap: 0.3rem;
    padding: 0.25rem 0.5rem;
  }

  .model-input {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-family: var(--font-mono);
    font-size: 0.76rem;
    padding: 0.25rem 0.4rem;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    background: var(--bg-surface);
    color: var(--text);
  }

  .model-input:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .use-btn {
    font: inherit;
    font-size: 0.72rem;
    padding: 0.25rem 0.5rem;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg-hover);
    color: var(--text);
    cursor: pointer;
  }

  .use-btn:hover {
    background: var(--accent-soft);
  }

  .use-btn:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }
</style>
