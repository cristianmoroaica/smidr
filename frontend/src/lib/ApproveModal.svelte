<script lang="ts">
  import type { SessionClient } from './ws';

  let {
    projectId,
    currentIteration,
    client,
    onClose,
    onInspect,
    mode = 'approve'
  }: {
    projectId: string;
    currentIteration: number | null;
    client: SessionClient;
    onClose: () => void;
    onInspect: () => void;
    mode?: 'approve' | 'export';
  } = $props();

  let dialogEl: HTMLDivElement | undefined = $state();

  let exporting = $state(false);
  let exportError = $state<string | null>(null);
  let exportResult = $state<{ dir: string; files: { name: string; url: string }[] } | null>(null);

  let openingFolder = $state(false);
  let openFolderError = $state<string | null>(null);

  let lockingBaseline = $state(false);
  let lockError = $state<string | null>(null);

  $effect(() => {
    dialogEl?.focus();
    function onKeydown(e: KeyboardEvent) {
      if (e.key === 'Escape') onClose();
    }
    window.addEventListener('keydown', onKeydown);
    return () => window.removeEventListener('keydown', onKeydown);
  });

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onClose();
  }

  async function triggerDownload(file: { name: string; url: string }) {
    const r = await fetch(file.url);
    if (!r.ok) return;
    const blob = await r.blob();
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = file.name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(a.href);
  }

  async function doExport() {
    if (exporting) return;
    exporting = true;
    exportError = null;
    try {
      const res = await fetch(`/api/projects/${encodeURIComponent(projectId)}/export`, {
        method: 'POST'
      });
      if (!res.ok) throw new Error(`Export failed: ${res.status}`);
      const data = (await res.json()) as { dir: string; files: { name: string; url: string }[] };
      exportResult = data;
      for (const f of data.files) {
        try {
          await triggerDownload(f);
        } catch {
          // ignore a single failed download; the confirmation below still
          // shows the destination so the user can retrieve files manually.
        }
      }
    } catch (e) {
      exportError = e instanceof Error ? e.message : String(e);
    } finally {
      exporting = false;
    }
  }

  async function doOpenFolder() {
    if (openingFolder) return;
    openingFolder = true;
    openFolderError = null;
    try {
      const res = await fetch(`/api/projects/${encodeURIComponent(projectId)}/open-folder`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ target: 'exports' })
      });
      if (!res.ok) throw new Error(`Open folder failed: ${res.status}`);
    } catch (e) {
      openFolderError = e instanceof Error ? e.message : String(e);
    } finally {
      openingFolder = false;
    }
  }

  async function doLockBaseline() {
    if (lockingBaseline) return;
    lockingBaseline = true;
    lockError = null;
    try {
      if (currentIteration !== null) {
        const res = await fetch(`/api/projects/${encodeURIComponent(projectId)}/baseline`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ n: currentIteration })
        });
        if (!res.ok) throw new Error(`Baseline failed: ${res.status}`);
      }
      client.send({ type: 'approve_phase' });
      client.send({ type: 'advance' });
      onClose();
    } catch (e) {
      lockError = e instanceof Error ? e.message : String(e);
    } finally {
      lockingBaseline = false;
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={onBackdropClick}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="approve-modal-title"
    tabindex="-1"
    bind:this={dialogEl}
  >
    <div class="dialog-header">
      <h2 id="approve-modal-title">{mode === 'export' ? 'Export' : 'Approve build'}</h2>
      <button type="button" class="close-btn" aria-label="Close" onclick={onClose}>×</button>
    </div>

    <div class="cards">
      <button type="button" class="option-card" onclick={doExport} disabled={exporting}>
        <span class="option-title">{exporting ? 'Exporting…' : 'Export build'}</span>
        <span class="option-desc">Download the current iteration as STL and STEP.</span>
      </button>

      {#if exportError}
        <div class="inline-error">{exportError}</div>
      {/if}

      {#if exportResult}
        <div class="confirmation">
          <div class="confirmation-label">Exported to</div>
          <div class="confirmation-path">{exportResult.dir}</div>
          <button
            type="button"
            class="folder-btn"
            onclick={doOpenFolder}
            disabled={openingFolder}
          >
            {openingFolder ? 'Opening…' : 'Open folder'}
          </button>
          {#if openFolderError}
            <div class="inline-error">{openFolderError}</div>
          {/if}
        </div>
      {/if}

      {#if mode === 'approve'}
      <button type="button" class="option-card" onclick={onInspect}>
        <span class="option-title">Inspect components</span>
        <span class="option-desc">Open the Parts panel to isolate individual components.</span>
      </button>

      <button
        type="button"
        class="option-card"
        onclick={doLockBaseline}
        disabled={lockingBaseline}
      >
        <span class="option-title"
          >{lockingBaseline ? 'Locking…' : 'Lock as baseline & refine'}</span
        >
        <span class="option-desc"
          >Record this iteration as the baseline and move to Refine.</span
        >
      </button>

      {#if lockError}
        <div class="inline-error">{lockError}</div>
      {/if}
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
    padding: 1.5rem;
  }

  .dialog {
    width: 100%;
    max-width: 560px;
    max-height: 85vh;
    overflow-y: auto;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-2);
    padding: 1.25rem;
  }

  .dialog:focus-visible,
  .dialog:focus {
    outline: none;
  }

  .dialog-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 1rem;
  }

  .dialog-header h2 {
    margin: 0;
    font-size: 1.05rem;
    font-weight: 650;
    color: var(--text);
  }

  .close-btn {
    font: inherit;
    font-size: 1.2rem;
    line-height: 1;
    background: transparent;
    border: none;
    color: var(--text-secondary);
    cursor: pointer;
    padding: 0.2rem 0.5rem;
    border-radius: var(--radius-sm);
  }

  .close-btn:hover {
    background: var(--bg-hover);
    color: var(--text);
  }

  .close-btn:focus-visible {
    box-shadow: var(--focus-ring);
  }

  .cards {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .option-card {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    text-align: left;
    font: inherit;
    padding: 0.75rem 1rem;
    background: var(--bg-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text);
    cursor: pointer;
    transition: background 120ms, border-color 120ms;
  }

  .option-card:hover:not(:disabled) {
    background: var(--bg-hover);
    border-color: var(--border-strong);
  }

  .option-card:focus-visible {
    box-shadow: var(--focus-ring);
  }

  .option-card:disabled {
    cursor: default;
    opacity: 0.7;
  }

  .option-title {
    font-weight: 600;
  }

  .option-desc {
    font-size: 0.82rem;
    color: var(--text-secondary);
  }

  .inline-error {
    font-size: 0.82rem;
    color: var(--danger);
    padding: 0.1rem 0.2rem;
  }

  .confirmation {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    padding: 0.7rem 0.9rem;
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
  }

  .confirmation-label {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-secondary);
  }

  .confirmation-path {
    font-family: var(--font-mono);
    font-size: 0.8rem;
    color: var(--text);
    overflow-wrap: break-word;
    word-break: break-all;
    max-height: 4.5rem;
    overflow-y: auto;
  }

  .folder-btn {
    align-self: flex-start;
    font: inherit;
    font-size: 0.8rem;
    font-weight: 600;
    padding: 0.35rem 0.75rem;
    background: var(--accent);
    color: var(--accent-fg);
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }

  .folder-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .folder-btn:focus-visible {
    box-shadow: var(--focus-ring);
  }

  .folder-btn:disabled {
    cursor: default;
    opacity: 0.7;
  }
</style>
