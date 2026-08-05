<script lang="ts">
  import { renderMarkdown as render } from './markdown';
  import { tick } from 'svelte';
  import RefPicker from './RefPicker.svelte';

  type Message = { role: string; content: string };
  type ToolCall = { name: string; detail: string };

  let {
    messages,
    streaming,
    toolCalls,
    busy,
    selectedParts,
    libRefs,
    pendingQuestion,
    onSend,
    onCancel,
    onRemovePart,
    onAddRef,
    onRemoveRef
  }: {
    messages: Message[];
    streaming: string;
    toolCalls: ToolCall[];
    busy: boolean;
    selectedParts: string[];
    libRefs: string[];
    pendingQuestion: { question: string; options: string[] } | null;
    onSend: (text: string) => void;
    onCancel: () => void;
    onRemovePart: (name: string) => void;
    onAddRef: (slug: string) => void;
    onRemoveRef: (slug: string) => void;
  } = $props();

  let draft = $state('');
  let logEl: HTMLDivElement | undefined = $state();
  let textareaEl: HTMLTextAreaElement | undefined = $state();
  let refQuery = $state<string | null>(null);
  let refPicker: RefPicker | undefined = $state();

  const REF_TRIGGER_RE = /(?:^|\s)\/ref\s*([\w-]*)$/;

  function updateRefTrigger() {
    const caret = textareaEl?.selectionStart ?? draft.length;
    const upToCaret = draft.slice(0, caret);
    const m = REF_TRIGGER_RE.exec(upToCaret);
    refQuery = m ? m[1] : null;
  }

  function chooseRef(slug: string) {
    const caret = textareaEl?.selectionStart ?? draft.length;
    const upToCaret = draft.slice(0, caret);
    const m = REF_TRIGGER_RE.exec(upToCaret);
    if (m) {
      const matchStart = m.index + (m[0].startsWith(' ') ? 1 : 0);
      draft = draft.slice(0, matchStart) + draft.slice(caret);
    }
    onAddRef(slug);
    refQuery = null;
  }

  function scrollToBottom() {
    if (logEl) logEl.scrollTop = logEl.scrollHeight;
  }

  $effect(() => {
    // Re-run whenever messages, streaming, toolCalls, or pendingQuestion change.
    void messages;
    void streaming;
    void toolCalls;
    void pendingQuestion;
    tick().then(scrollToBottom);
  });

  // Index of the last role:'question' history entry whose text matches the
  // outstanding pending question, or -1. That entry is the history copy of
  // the live card, so it gets skipped and the live card (with chips) stands
  // in its place. Deliberately NOT "is it the last message": the server
  // appends the model's closing assistant text after the question entry in a
  // real streamed turn (the tool_use event lands ticks before the result),
  // so a positional check would let the question render twice.
  const liveQuestionIndex = $derived.by(() => {
    if (!pendingQuestion) return -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      const m = messages[i];
      if (m.role === 'question' && m.content === pendingQuestion.question) return i;
    }
    return -1;
  });

  function submit() {
    const text = draft.trim();
    if (!text) return;
    onSend(text);
    draft = '';
  }

  function onKeydown(e: KeyboardEvent) {
    if (refQuery !== null && refPicker?.handleKeydown(e)) {
      return;
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  }

  function onDraftInput() {
    updateRefTrigger();
  }

  function dismissRefPicker() {
    refQuery = null;
  }
</script>

<div class="chat">
  <div class="log" bind:this={logEl}>
    {#each messages as m, i}
      {#if m.role === 'question'}
        {#if i !== liveQuestionIndex}
          <div class="question-card">
            <div class="question-label">Question</div>
            <div class="content">{@html render(m.content)}</div>
          </div>
        {/if}
      {:else}
        <div class="bubble {m.role}">
          <div class="role-label">{m.role}</div>
          <div class="content">{@html render(m.content)}</div>
        </div>
      {/if}
    {/each}

    {#if streaming}
      <div class="bubble assistant streaming">
        <div class="role-label">assistant</div>
        <div class="content">{@html render(streaming)}</div>
      </div>
    {/if}

    {#each toolCalls as tc}
      <details class="tool-call">
        <summary>{tc.name}</summary>
        <pre class="tool-detail">{tc.detail}</pre>
      </details>
    {/each}

    {#if pendingQuestion}
      <div class="question-card">
        <div class="question-label">Question</div>
        <div class="content">{@html render(pendingQuestion.question)}</div>
        {#if pendingQuestion.options.length > 0}
          <div class="question-options">
            {#each pendingQuestion.options as opt}
              <button class="option-chip" onclick={() => onSend(opt)}>{opt}</button>
            {/each}
          </div>
          <p class="question-hint">or type your own answer below</p>
        {/if}
      </div>
    {/if}
  </div>

  <div class="composer-wrap">
    {#if selectedParts.length > 0 || libRefs.length > 0}
      <div class="chips">
        {#each selectedParts as name}
          <span class="chip">
            {name}
            <button class="chip-remove" onclick={() => onRemovePart(name)} aria-label="Remove {name}"
              >×</button
            >
          </span>
        {/each}
        {#each libRefs as slug}
          <span class="chip ref">
            {slug}
            <button
              class="chip-remove"
              onclick={() => onRemoveRef(slug)}
              aria-label="Remove reference {slug}">×</button
            >
          </span>
        {/each}
      </div>
    {/if}
    <div class="composer">
      <div class="textarea-wrap">
        {#if refQuery !== null}
          <div class="ref-picker-anchor">
            <RefPicker
              bind:this={refPicker}
              query={refQuery}
              onChoose={chooseRef}
              onDismiss={dismissRefPicker}
            />
          </div>
        {/if}
        <textarea
          bind:this={textareaEl}
          bind:value={draft}
          onkeydown={onKeydown}
          oninput={onDraftInput}
          onclick={updateRefTrigger}
          placeholder="Type a message... (Enter to send, Shift+Enter for newline, /ref to attach a reference)"
          rows="3"
        ></textarea>
      </div>
      <div class="composer-actions">
        {#if busy}
          <button class="cancel" onclick={onCancel}>Cancel</button>
        {/if}
        <button class="send" onclick={submit} disabled={!draft.trim()}>Send</button>
      </div>
    </div>
  </div>
</div>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--bg-surface, #1a1d24);
    color: var(--text, #e6e9ef);
  }

  .log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 1rem 0.9rem;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }

  .bubble {
    max-width: 88%;
    padding: 0.55rem 0.8rem;
    border-radius: var(--radius-md, 10px);
    line-height: 1.5;
    font-size: 0.875rem;
    overflow-wrap: anywhere;
  }

  .bubble.user {
    align-self: flex-end;
    background: var(--accent-soft, rgba(79, 143, 247, 0.14));
    border: 1px solid var(--accent-border, rgba(79, 143, 247, 0.45));
    color: var(--text, #e6e9ef);
  }

  .bubble.assistant {
    align-self: flex-start;
    background: var(--bg-raised, #22262f);
    border: 1px solid var(--border, #2e333d);
    color: var(--text, #e6e9ef);
  }

  .bubble.system {
    position: relative;
    align-self: stretch;
    max-width: 100%;
    background: transparent;
    border: none;
    border-left: 2px solid var(--border-strong, #3d434f);
    border-radius: 0;
    padding: 0.15rem 0 0.15rem 0.6rem;
    color: var(--text-secondary, #9aa3b2);
    font-style: normal;
    font-size: 0.8rem;
  }

  .bubble.streaming {
    opacity: 0.9;
  }

  .role-label {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted, #6b7280);
    opacity: 1;
    margin-bottom: 0.25rem;
    font-weight: 600;
  }

  .bubble.system .role-label {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .content :global(p) {
    margin: 0.3em 0;
  }

  .content :global(code) {
    background: rgba(255, 255, 255, 0.07);
    padding: 0.05em 0.3em;
    border-radius: 4px;
    font-family: var(--font-mono, monospace);
  }

  .content :global(pre) {
    overflow-x: auto;
    padding: 0.6em 0.7em;
    background: var(--bg-inset, #0d0f13);
    border: 1px solid var(--border, #2e333d);
    border-radius: var(--radius-sm, 6px);
  }

  .content :global(pre code) {
    background: none;
    padding: 0;
  }

  .content :global(ul),
  .content :global(ol) {
    margin: 0.35em 0;
    padding-left: 1.15em;
  }

  .content :global(li) {
    margin: 0.15em 0;
  }

  .content :global(a) {
    color: var(--accent, #4f8ff7);
  }

  .question-card {
    align-self: stretch;
    max-width: 100%;
    background: var(--bg-raised, #22262f);
    border: 1px solid var(--border, #2e333d);
    border-left: 3px solid var(--accent, #4f8ff7);
    border-radius: var(--radius-md, 10px);
    padding: 0.55rem 0.8rem;
  }

  .question-label {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--accent, #4f8ff7);
    font-weight: 600;
    margin-bottom: 0.25rem;
  }

  .question-options {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    margin-top: 0.5rem;
  }

  .option-chip {
    font: inherit;
    font-size: 0.8rem;
    padding: 0.35rem 0.8rem;
    border-radius: var(--radius-pill, 999px);
    background: var(--bg-inset, #0d0f13);
    border: 1px solid var(--accent-border, rgba(79, 143, 247, 0.45));
    color: var(--accent, #4f8ff7);
    cursor: pointer;
    transition: background 120ms;
  }

  .option-chip:hover {
    background: var(--accent-soft, rgba(79, 143, 247, 0.14));
  }

  .option-chip:focus-visible {
    box-shadow: var(--focus-ring, 0 0 0 2px rgba(79, 143, 247, 0.5));
    outline: none;
  }

  .question-hint {
    margin: 0.4rem 0 0;
    color: var(--text-muted, #6b7280);
    font-size: 0.75rem;
  }

  .tool-call {
    align-self: stretch;
    background: var(--bg-inset, #0d0f13);
    border: 1px solid var(--border, #2e333d);
    border-radius: var(--radius-sm, 6px);
    padding: 0.35rem 0.6rem;
    font-size: 0.8rem;
    color: var(--text-secondary, #9aa3b2);
  }

  .tool-call summary {
    cursor: pointer;
    font-weight: 600;
    color: var(--text, #e6e9ef);
    list-style: none;
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }

  .tool-call summary::before {
    content: '▸';
    color: var(--text-muted, #6b7280);
    transition: transform 120ms;
  }

  .tool-call[open] > summary::before {
    transform: rotate(90deg);
  }

  .tool-call summary::-webkit-details-marker {
    display: none;
  }

  .tool-detail {
    font-family: var(--font-mono, monospace);
    font-size: 0.75rem;
    color: var(--text-secondary, #9aa3b2);
    white-space: pre-wrap;
    overflow-x: auto;
    margin: 0.4rem 0 0 0;
  }

  .composer-wrap {
    border-top: 1px solid var(--border, #2e333d);
    background: var(--bg-surface, #1a1d24);
  }

  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    padding: 0.5rem 0.75rem 0;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    background: var(--bg-raised, #22262f);
    border: 1px solid var(--border-strong, #3d434f);
    color: var(--text, #e6e9ef);
    padding: 0.1rem 0.25rem 0.1rem 0.6rem;
    border-radius: var(--radius-pill, 999px);
    font-size: 0.75rem;
  }

  .chip.ref {
    border-color: var(--success, #3fb950);
    color: var(--success, #3fb950);
  }

  .chip:not(.ref) {
    border-color: var(--accent-border, rgba(79, 143, 247, 0.45));
    color: var(--accent, #4f8ff7);
  }

  .textarea-wrap {
    position: relative;
    flex: 1;
    min-width: 0;
    display: flex;
  }

  .ref-picker-anchor {
    position: absolute;
    bottom: 100%;
    left: 0;
    right: 0;
    margin-bottom: 0.3rem;
    z-index: 10;
  }

  .chip-remove {
    background: transparent;
    border: none;
    color: inherit;
    opacity: 0.7;
    font: inherit;
    font-size: 0.9rem;
    line-height: 1;
    padding: 0.1rem 0.3rem;
    cursor: pointer;
    border-radius: 50%;
  }

  .chip-remove:hover {
    opacity: 1;
    background: rgba(255, 255, 255, 0.12);
  }

  .composer {
    display: flex;
    gap: 0.5rem;
    padding: 0.75rem;
    align-items: flex-end;
  }

  textarea {
    width: 100%;
    resize: vertical;
    font: inherit;
    padding: 0.6rem 0.7rem;
    line-height: 1.5;
    min-height: 4.5rem;
    border-radius: var(--radius-md, 10px);
    border: 1px solid var(--border-strong, #3d434f);
    background: var(--bg-inset, #0d0f13);
    color: var(--text, #e6e9ef);
  }

  textarea::placeholder {
    color: var(--text-muted, #6b7280);
  }

  textarea:focus {
    outline: none;
    border-color: var(--accent, #4f8ff7);
    box-shadow: var(--focus-ring, 0 0 0 2px rgba(79, 143, 247, 0.5));
  }

  .composer-actions {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  button {
    font: inherit;
    padding: 0.5rem 0.9rem;
    border-radius: var(--radius-sm, 6px);
    border: 1px solid var(--border-strong, #3d434f);
    background: var(--bg-raised, #22262f);
    color: var(--text, #e6e9ef);
    cursor: pointer;
  }

  button:focus-visible {
    box-shadow: var(--focus-ring, 0 0 0 2px rgba(79, 143, 247, 0.5));
  }

  button:disabled {
    background: transparent;
    color: var(--text-muted, #6b7280);
    border-color: var(--border, #2e333d);
    cursor: not-allowed;
    opacity: 1;
  }

  button.send {
    background: var(--accent, #4f8ff7);
    color: var(--accent-fg, #0b1220);
    border-color: transparent;
    font-weight: 600;
  }

  button.send:hover:not(:disabled) {
    background: var(--accent-hover, #6ba1f9);
  }

  button.cancel {
    background: transparent;
    color: var(--danger, #f05252);
    border-color: var(--danger, #f05252);
  }

  button.cancel:hover {
    background: var(--danger-soft, rgba(240, 82, 82, 0.14));
  }
</style>
