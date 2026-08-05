<script lang="ts">
  const PHASES = ['spec', 'build', 'refine'] as const;
  type Phase = (typeof PHASES)[number];

  let {
    phase,
    approved,
    onApprove,
    onAdvance,
    onBack
  }: {
    phase: string;
    approved: boolean;
    onApprove: () => void;
    onAdvance: () => void;
    onBack: (target: 'spec' | 'build') => void;
  } = $props();

  function normalize(p: string): Phase {
    const lower = p.toLowerCase();
    return (PHASES as readonly string[]).includes(lower) ? (lower as Phase) : 'spec';
  }

  let currentIndex = $derived(PHASES.indexOf(normalize(phase)));
  let isRefine = $derived(normalize(phase) === 'refine');
  // Cosmetic only: server is authoritative and re-validates every action.
  let advanceDisabled = $derived(!approved || isRefine);
</script>

<div class="stepper">
  <div class="phases">
    {#each PHASES as p, i}
      <div class="phase" class:current={i === currentIndex} class:past={i < currentIndex}>
        <span class="dot">{i + 1}</span>
        <span class="label">{p[0].toUpperCase()}{p.slice(1)}</span>
        {#if i < currentIndex}
          <button
            class="back"
            onclick={() => onBack(p as 'spec' | 'build')}
            title="Go back to {p}"
          >
            back
          </button>
        {/if}
      </div>
      {#if i < PHASES.length - 1}
        <div class="connector"></div>
      {/if}
    {/each}
  </div>

  <div class="actions">
    <button class="approve" onclick={onApprove} disabled={approved}>
      {approved ? 'Approved' : 'Approve'}
    </button>
    <button class="advance" onclick={onAdvance} disabled={advanceDisabled}>Advance</button>
  </div>
</div>

<style>
  .stepper {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.6rem 1rem;
    background: var(--bg-app);
    border-bottom: 1px solid var(--border);
    flex-wrap: wrap;
  }

  .phases {
    display: flex;
    align-items: center;
    gap: 0.25rem;
  }

  .phase {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.25rem 0.7rem;
    border: 1px solid transparent;
    border-radius: var(--radius-pill);
  }

  .phase .label {
    color: var(--text-muted);
  }

  .phase .dot {
    background: transparent;
    border: 1px solid var(--border-strong);
    color: var(--text-muted);
  }

  .phase.current {
    background: var(--accent-soft);
    border: 1px solid var(--accent-border);
  }

  .phase.current .label {
    color: var(--text);
    font-weight: 600;
  }

  .phase.current .dot {
    background: var(--accent);
    color: var(--accent-fg);
    border-color: transparent;
  }

  .phase.past .label {
    color: var(--text-secondary);
  }

  .phase.past .dot {
    background: var(--success-soft);
    color: var(--success);
    border: 1px solid var(--success);
  }

  .dot {
    width: 1.25rem;
    height: 1.25rem;
    border-radius: 50%;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 0.7rem;
    font-weight: 600;
    flex: 0 0 auto;
  }

  .label {
    font-size: 0.85rem;
    letter-spacing: 0.01em;
    white-space: nowrap;
  }

  .connector {
    width: 1.5rem;
    height: 1px;
    background: var(--border);
  }

  .back {
    font-size: 0.7rem;
    padding: 0.1rem 0.45rem;
    border-radius: var(--radius-sm);
    background: transparent;
    border: 1px solid var(--border-strong);
    color: var(--text-secondary);
    line-height: 1.4;
    cursor: pointer;
    transition: background 120ms, border-color 120ms, color 120ms;
  }

  .back:hover {
    color: var(--text);
    border-color: var(--accent);
    background: var(--accent-soft);
  }

  .actions {
    display: flex;
    gap: 0.5rem;
  }

  button {
    font: inherit;
    padding: 0.4rem 0.9rem;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border-strong);
    background: var(--bg-raised);
    color: var(--text);
    cursor: pointer;
    font-weight: 500;
    transition: background 120ms, border-color 120ms;
  }

  button:focus-visible {
    box-shadow: var(--focus-ring);
  }

  button:disabled {
    background: transparent;
    color: var(--text-muted);
    border-color: var(--border);
    cursor: not-allowed;
    opacity: 1;
  }

  button.approve:not(:disabled) {
    background: var(--success);
    color: var(--success-fg);
    border-color: transparent;
  }

  button.approve:not(:disabled):hover {
    background: var(--success-hover);
  }

  button.approve:disabled {
    color: var(--success);
    border-color: var(--success);
  }

  button.advance:not(:disabled) {
    background: var(--accent);
    color: var(--accent-fg);
    border-color: transparent;
  }

  button.advance:not(:disabled):hover {
    background: var(--accent-hover);
  }
</style>
