<script lang="ts">
  import { marked } from 'marked';
  import { tick } from 'svelte';

  type Message = { role: string; content: string };
  type ToolCall = { name: string; detail: string };

  let {
    messages,
    streaming,
    toolCalls,
    busy,
    onSend,
    onCancel
  }: {
    messages: Message[];
    streaming: string;
    toolCalls: ToolCall[];
    busy: boolean;
    onSend: (text: string) => void;
    onCancel: () => void;
  } = $props();

  let draft = $state('');
  let logEl: HTMLDivElement | undefined = $state();

  function render(content: string): string {
    return marked.parse(content, { async: false }) as string;
  }

  function scrollToBottom() {
    if (logEl) logEl.scrollTop = logEl.scrollHeight;
  }

  $effect(() => {
    // Re-run whenever messages, streaming, or toolCalls change.
    void messages;
    void streaming;
    void toolCalls;
    tick().then(scrollToBottom);
  });

  function submit() {
    const text = draft.trim();
    if (!text) return;
    onSend(text);
    draft = '';
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  }
</script>

<div class="chat">
  <div class="log" bind:this={logEl}>
    {#each messages as m}
      <div class="bubble {m.role}">
        <div class="role-label">{m.role}</div>
        <div class="content">{@html render(m.content)}</div>
      </div>
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
  </div>

  <div class="composer">
    <textarea
      bind:value={draft}
      onkeydown={onKeydown}
      placeholder="Type a message... (Enter to send, Shift+Enter for newline)"
      rows="3"
    ></textarea>
    <div class="composer-actions">
      {#if busy}
        <button class="cancel" onclick={onCancel}>Cancel</button>
      {/if}
      <button class="send" onclick={submit} disabled={!draft.trim()}>Send</button>
    </div>
  </div>
</div>

<style>
  .chat {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }

  .log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .bubble {
    max-width: 80%;
    padding: 0.6rem 0.9rem;
    border-radius: 0.5rem;
    line-height: 1.4;
  }

  .bubble.user {
    align-self: flex-end;
    background: var(--bubble-user-bg, #2b5fd9);
    color: var(--bubble-user-fg, #fff);
  }

  .bubble.assistant {
    align-self: flex-start;
    background: var(--bubble-assistant-bg, #24262b);
    color: var(--bubble-assistant-fg, #e8e8ea);
  }

  .bubble.system {
    align-self: center;
    background: var(--bubble-system-bg, #3a3a3f);
    color: var(--bubble-system-fg, #c9c9cd);
    font-style: italic;
    font-size: 0.85rem;
  }

  .bubble.streaming {
    opacity: 0.85;
  }

  .role-label {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    opacity: 0.6;
    margin-bottom: 0.2rem;
  }

  .content :global(p) {
    margin: 0.3em 0;
  }

  .content :global(pre) {
    overflow-x: auto;
    padding: 0.5em;
    background: rgba(0, 0, 0, 0.25);
    border-radius: 0.3em;
  }

  .tool-call {
    align-self: stretch;
    background: var(--tool-call-bg, #1b1c20);
    border: 1px solid var(--tool-call-border, #35363c);
    border-radius: 0.4rem;
    padding: 0.4rem 0.6rem;
    font-size: 0.85rem;
  }

  .tool-call summary {
    cursor: pointer;
    font-weight: 600;
  }

  .tool-detail {
    white-space: pre-wrap;
    overflow-x: auto;
    margin: 0.4rem 0 0 0;
  }

  .composer {
    display: flex;
    gap: 0.5rem;
    padding: 0.75rem;
    border-top: 1px solid var(--border, #333);
    align-items: flex-end;
  }

  textarea {
    flex: 1;
    resize: vertical;
    font: inherit;
    padding: 0.5rem;
    border-radius: 0.4rem;
    border: 1px solid var(--border, #444);
    background: var(--input-bg, #1a1b1e);
    color: inherit;
  }

  .composer-actions {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  button {
    font: inherit;
    padding: 0.5rem 0.9rem;
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

  button.send {
    background: var(--accent, #2b5fd9);
    color: #fff;
    border-color: transparent;
  }

  button.cancel {
    background: var(--danger, #a83232);
    color: #fff;
    border-color: transparent;
  }
</style>
