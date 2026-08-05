<script lang="ts">
  import type { SessionClient } from './ws';

  let {
    phase,
    target,
    reason,
    client,
    onDismiss
  }: {
    phase: string;
    target: string;
    reason: string;
    client: SessionClient;
    onDismiss: () => void;
  } = $props();

  const PHASE_ORDER = ['spec', 'build', 'refine'];

  function capitalize(s: string): string {
    return s.length === 0 ? s : s[0].toUpperCase() + s.slice(1);
  }

  let dialogEl: HTMLDivElement | undefined = $state();

  $effect(() => {
    dialogEl?.focus();
    function onKeydown(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        // Capture-phase + stopPropagation: this modal stacks above the
        // approve modal, so one Esc must not dismiss both.
        e.stopPropagation();
        deny();
      }
    }
    window.addEventListener('keydown', onKeydown, true);
    return () => window.removeEventListener('keydown', onKeydown, true);
  });

  function onBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) deny();
  }

  function deny() {
    client.send({ type: 'deny_phase_switch' });
    onDismiss();
  }

  function switchPhase() {
    const currentIdx = PHASE_ORDER.indexOf(phase.toLowerCase());
    const targetIdx = PHASE_ORDER.indexOf(target.toLowerCase());
    if (targetIdx !== -1 && currentIdx !== -1) {
      if (targetIdx < currentIdx) {
        client.send({ type: 'go_back', target: target.toLowerCase() as 'spec' | 'build' });
      } else if (targetIdx > currentIdx) {
        client.send({ type: 'approve_phase' });
        client.send({ type: 'advance' });
      }
    }
    onDismiss();
  }
</script>

<div class="backdrop" role="presentation" onclick={onBackdropClick}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="phase-switch-modal-title"
    tabindex="-1"
    bind:this={dialogEl}
  >
    <h2 id="phase-switch-modal-title">
      The agent requests switching to {capitalize(target)}
    </h2>
    <p class="reason">{reason}</p>

    <div class="actions">
      <button type="button" class="btn-primary" onclick={switchPhase}>
        Switch to {capitalize(target)}
      </button>
      <button type="button" class="btn-secondary" onclick={deny}>
        Stay in {capitalize(phase)}
      </button>
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
    z-index: 200;
    padding: 1.5rem;
  }

  .dialog {
    width: 100%;
    max-width: 480px;
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

  .dialog h2 {
    margin: 0 0 0.6rem;
    font-size: 1.05rem;
    font-weight: 650;
    color: var(--text);
  }

  .reason {
    margin: 0 0 1.1rem;
    color: var(--text-secondary);
    font-size: 0.9rem;
    line-height: 1.4;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.6rem;
  }

  .btn-primary,
  .btn-secondary {
    font: inherit;
    font-weight: 600;
    padding: 0.5rem 1rem;
    border-radius: var(--radius-md);
    cursor: pointer;
    border: 1px solid transparent;
  }

  .btn-primary {
    background: var(--accent);
    color: var(--accent-fg);
  }

  .btn-primary:hover {
    background: var(--accent-hover);
  }

  .btn-primary:focus-visible,
  .btn-secondary:focus-visible {
    box-shadow: var(--focus-ring);
  }

  .btn-secondary {
    background: var(--bg-raised);
    border-color: var(--border);
    color: var(--text);
  }

  .btn-secondary:hover {
    background: var(--bg-hover);
    border-color: var(--border-strong);
  }
</style>
