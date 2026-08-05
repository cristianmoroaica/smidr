<script lang="ts">
  import { renderMarkdown as render } from './markdown';

  let {
    spec,
    phase,
    approved,
    onApprove
  }: {
    spec: string | null;
    phase: string;
    approved: boolean;
    onApprove: () => void;
  } = $props();

  type Section = { heading: string | null; body: string };

  function splitSections(text: string): Section[] {
    const headingRe = /^#{1,6} .*$/gm;
    const matches = [...text.matchAll(headingRe)];
    if (matches.length === 0) {
      return [{ heading: null, body: text }];
    }
    const sections: Section[] = [];
    const firstStart = matches[0].index ?? 0;
    if (firstStart > 0) {
      const leading = text.slice(0, firstStart);
      if (leading.trim()) sections.push({ heading: null, body: leading });
    }
    for (let i = 0; i < matches.length; i++) {
      const m = matches[i];
      const start = m.index ?? 0;
      const end = i + 1 < matches.length ? matches[i + 1].index ?? text.length : text.length;
      const heading = m[0].replace(/^#{1,6}\s+/, '').trim();
      const body = text.slice(start + m[0].length, end);
      sections.push({ heading, body });
    }
    return sections;
  }

  let sections = $derived(spec && spec.trim() ? splitSections(spec) : []);
  let showApprove = $derived(phase.toLowerCase() === 'spec');
</script>

<div class="spec-panel">
  <div class="header">
    <span class="title">Spec</span>
    {#if showApprove}
      <button class="approve" onclick={onApprove} disabled={approved}>
        {approved ? 'Approved ✓' : 'Approve spec'}
      </button>
    {/if}
  </div>
  <div class="body">
    {#if sections.length === 0}
      <p class="muted">No spec yet</p>
    {:else}
      {#each sections as section}
        {#if section.heading === null}
          <div class="section leading">{@html render(section.body)}</div>
        {:else}
          <details class="section" open>
            <summary>{section.heading}</summary>
            <div class="section-body">{@html render(section.body)}</div>
          </details>
        {/if}
      {/each}
    {/if}
  </div>
</div>

<style>
  .spec-panel {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
    background: var(--bg-surface, #1a1d24);
    color: var(--text, #e6e9ef);
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.8rem;
    background: var(--bg-app, #111318);
    border-bottom: 1px solid var(--border, #2e333d);
    flex: 0 0 auto;
  }

  .title {
    font-size: 0.75rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-secondary, #9aa3b2);
  }

  .approve {
    font: inherit;
    padding: 0.3rem 0.7rem;
    border-radius: var(--radius-sm, 6px);
    border: 1px solid transparent;
    background: var(--success, #3fb950);
    color: var(--success-fg, #08130b);
    cursor: pointer;
    font-weight: 600;
    font-size: 0.78rem;
  }

  .approve:not(:disabled):hover {
    background: var(--success-hover, #35a344);
  }

  .approve:disabled {
    background: transparent;
    color: var(--success, #3fb950);
    border-color: var(--success, #3fb950);
    cursor: not-allowed;
    opacity: 1;
  }

  .body {
    overflow-y: auto;
    min-height: 0;
    flex: 1;
    padding: 0.75rem 0.9rem 1.25rem;
  }

  .muted {
    color: var(--text-muted, #6b7280);
    opacity: 1;
    font-size: 0.85rem;
  }

  .section {
    font-size: 0.82rem;
    margin-bottom: 0.35rem;
    border-bottom: 1px solid var(--border, #2e333d);
    padding-bottom: 0.35rem;
  }

  .section summary {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    cursor: pointer;
    font-weight: 600;
    font-size: 0.8rem;
    color: var(--text, #e6e9ef);
    padding: 0.3rem 0;
    list-style: none;
  }

  .section summary::-webkit-details-marker {
    display: none;
  }

  .section summary::before {
    content: '▸';
    color: var(--text-muted, #6b7280);
    transition: transform 120ms;
  }

  .section[open] > summary::before {
    transform: rotate(90deg);
  }

  .section-body {
    padding: 0.1rem 0 0.5rem 0.9rem;
    color: var(--text-secondary, #9aa3b2);
  }

  .section :global(h1),
  .section :global(h2),
  .section :global(h3),
  .section :global(h4),
  .section :global(h5),
  .section :global(h6) {
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--text, #e6e9ef);
    margin: 0.7em 0 0.3em;
  }

  .section :global(p) {
    margin: 0.35em 0;
    line-height: 1.6;
  }

  .section :global(ul),
  .section :global(ol) {
    margin: 0.35em 0;
    padding-left: 1.2em;
  }

  .section :global(li) {
    margin: 0.2em 0;
  }

  .section :global(code) {
    font-family: var(--font-mono, monospace);
    background: rgba(255, 255, 255, 0.07);
    padding: 0.05em 0.3em;
    border-radius: 4px;
  }

  .section :global(pre) {
    background: var(--bg-inset, #0d0f13);
    border: 1px solid var(--border, #2e333d);
    border-radius: var(--radius-sm, 6px);
    padding: 0.6em 0.7em;
    overflow-x: auto;
  }

  .section :global(table) {
    border-collapse: collapse;
    width: 100%;
    font-size: 0.78rem;
  }

  .section :global(th),
  .section :global(td) {
    border: 1px solid var(--border, #2e333d);
    padding: 0.25rem 0.45rem;
    text-align: left;
  }

  .section :global(th) {
    background: var(--bg-raised, #22262f);
  }

  .section :global(hr) {
    border: none;
    border-top: 1px solid var(--border, #2e333d);
    margin: 0.8em 0;
  }

  .section :global(a) {
    color: var(--accent, #4f8ff7);
  }

  .section :global(strong) {
    color: var(--text, #e6e9ef);
  }

  .section :global(input[type='checkbox']) {
    appearance: none;
    width: 0.85em;
    height: 0.85em;
    border: 1px solid var(--border-strong, #3d434f);
    border-radius: 3px;
    background: var(--bg-inset, #0d0f13);
    vertical-align: -0.1em;
    margin-right: 0.45em;
    opacity: 1;
    position: relative;
  }

  .section :global(input[type='checkbox']:checked) {
    background: var(--success, #3fb950);
    border-color: var(--success, #3fb950);
  }

  .section :global(input[type='checkbox']:checked)::after {
    content: '✓';
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.7em;
    color: var(--success-fg, #08130b);
  }

  .section :global(li:has(input[type='checkbox'])) {
    list-style: none;
    margin-left: -1em;
  }
</style>
