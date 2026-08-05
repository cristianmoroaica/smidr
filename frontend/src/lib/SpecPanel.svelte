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
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 0.75rem;
    background: var(--button-bg, #2b2d31);
    border-bottom: 1px solid var(--border, #333);
    flex: 0 0 auto;
  }

  .title {
    font-weight: 600;
    font-size: 0.9rem;
  }

  .approve {
    font: inherit;
    padding: 0.3rem 0.7rem;
    border-radius: 0.4rem;
    border: 1px solid var(--border, #444);
    background: var(--accent, #2b5fd9);
    color: #fff;
    cursor: pointer;
    font-size: 0.8rem;
  }

  .approve:disabled {
    opacity: 0.6;
    cursor: not-allowed;
    background: var(--button-bg, #2b2d31);
    color: inherit;
  }

  .body {
    overflow-y: auto;
    min-height: 0;
    flex: 1;
    padding: 0.5rem 0.75rem;
  }

  .muted {
    opacity: 0.6;
    font-size: 0.85rem;
  }

  .section {
    margin-bottom: 0.5rem;
    font-size: 0.85rem;
  }

  .section summary {
    cursor: pointer;
    font-weight: 600;
    padding: 0.2rem 0;
  }

  .section-body {
    padding: 0.2rem 0 0.4rem 0.5rem;
  }

  .section :global(p) {
    margin: 0.3em 0;
  }

  .section :global(pre) {
    overflow-x: auto;
    padding: 0.5em;
    background: rgba(0, 0, 0, 0.25);
    border-radius: 0.3em;
  }
</style>
