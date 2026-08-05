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
        <span class="dot"></span>
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
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border, #333);
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
    gap: 0.4rem;
    padding: 0.25rem 0.5rem;
    border-radius: 0.4rem;
    opacity: 0.55;
  }

  .phase.current {
    opacity: 1;
    background: var(--phase-current-bg, #2b2d31);
  }

  .phase.past {
    opacity: 0.85;
  }

  .dot {
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background: var(--dot-bg, #666);
  }

  .phase.current .dot {
    background: var(--accent, #2b5fd9);
  }

  .connector {
    width: 1.5rem;
    height: 1px;
    background: var(--border, #444);
  }

  .back {
    font-size: 0.7rem;
    padding: 0.1rem 0.4rem;
    border-radius: 0.3rem;
  }

  .actions {
    display: flex;
    gap: 0.5rem;
  }

  button {
    font: inherit;
    padding: 0.4rem 0.8rem;
    border-radius: 0.4rem;
    border: 1px solid var(--border, #444);
    background: var(--button-bg, #2b2d31);
    color: inherit;
    cursor: pointer;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button.approve:not(:disabled) {
    background: var(--success, #2f8f4e);
    color: #fff;
    border-color: transparent;
  }

  button.advance:not(:disabled) {
    background: var(--accent, #2b5fd9);
    color: #fff;
    border-color: transparent;
  }
</style>
