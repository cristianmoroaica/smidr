<script lang="ts">
  import { renderMarkdown as render } from './markdown';
  import { tick } from 'svelte';
  import RefPicker from './RefPicker.svelte';

  type Message = { role: string; content: string };
  type ToolCall = { name: string; detail: string };
  type PendingImage = { key: string; file: File; previewUrl: string };

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
    onSend: (text: string, images?: File[]) => boolean | Promise<boolean>;
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
  let fileInputEl: HTMLInputElement | undefined = $state();
  let pendingImages = $state<PendingImage[]>([]);
  let attachmentError = $state<string | null>(null);
  let draggingImages = $state(false);
  let submitting = $state(false);

  const REF_TRIGGER_RE = /(?:^|\s)\/ref\s*([\w-]*)$/;
  const ACCEPTED_IMAGE_TYPES = new Set(['image/png', 'image/jpeg', 'image/webp', 'image/gif']);
  const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
  const MAX_IMAGES = 5;

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

  async function submit() {
    const text = draft.trim();
    if ((!text && pendingImages.length === 0) || submitting) return;
    submitting = true;
    try {
      const sent = await onSend(
        text,
        pendingImages.map((image) => image.file)
      );
      if (sent !== false) {
        draft = '';
        for (const image of pendingImages) URL.revokeObjectURL(image.previewUrl);
        pendingImages = [];
        attachmentError = null;
      }
    } finally {
      submitting = false;
    }
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

  function addImages(files: Iterable<File>) {
    attachmentError = null;
    const additions: PendingImage[] = [];
    for (const file of files) {
      const hasSupportedExtension = /\.(png|jpe?g|webp|gif)$/i.test(file.name);
      if (!ACCEPTED_IMAGE_TYPES.has(file.type) && !hasSupportedExtension) {
        attachmentError = 'Choose PNG, JPEG, WebP, or GIF images.';
        continue;
      }
      if (file.size > MAX_IMAGE_BYTES) {
        attachmentError = `${file.name} is larger than 10 MB.`;
        continue;
      }
      const duplicate = [...pendingImages, ...additions].some(
        (image) =>
          image.file.name === file.name &&
          image.file.size === file.size &&
          image.file.lastModified === file.lastModified
      );
      if (duplicate) continue;
      if (pendingImages.length + additions.length >= MAX_IMAGES) {
        attachmentError = `You can attach up to ${MAX_IMAGES} images.`;
        break;
      }
      additions.push({
        key: `${file.name}-${file.size}-${file.lastModified}`,
        file,
        previewUrl: URL.createObjectURL(file)
      });
    }
    if (additions.length > 0) pendingImages = [...pendingImages, ...additions];
    if (fileInputEl) fileInputEl.value = '';
  }

  function removeImage(key: string) {
    const image = pendingImages.find((candidate) => candidate.key === key);
    if (image) URL.revokeObjectURL(image.previewUrl);
    pendingImages = pendingImages.filter((candidate) => candidate.key !== key);
    attachmentError = null;
  }

  function onPaste(e: ClipboardEvent) {
    const images = Array.from(e.clipboardData?.files ?? []).filter((file) =>
      file.type.startsWith('image/')
    );
    if (images.length === 0) return;
    e.preventDefault();
    addImages(images);
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    draggingImages = false;
    addImages(Array.from(e.dataTransfer?.files ?? []));
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

  <div
    class="composer-wrap"
    class:dragging={draggingImages}
    role="group"
    aria-label="Message composer"
    ondragenter={(e) => {
      e.preventDefault();
      draggingImages = true;
    }}
    ondragover={(e) => e.preventDefault()}
    ondragleave={(e) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node | null)) draggingImages = false;
    }}
    ondrop={onDrop}
  >
    {#if pendingImages.length > 0}
      <div class="image-previews" aria-label="Attached images">
        {#each pendingImages as image (image.key)}
          <div class="image-preview">
            <img src={image.previewUrl} alt="Preview of {image.file.name}" />
            <span title={image.file.name}>{image.file.name}</span>
            <button
              class="image-remove"
              type="button"
              onclick={() => removeImage(image.key)}
              aria-label="Remove {image.file.name}"
            >×</button>
          </div>
        {/each}
      </div>
    {/if}
    {#if attachmentError}
      <div class="attachment-error" role="alert">{attachmentError}</div>
    {/if}
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
          onpaste={onPaste}
          placeholder="Type a message... (Enter to send, Shift+Enter for newline, /ref to attach a reference)"
          rows="3"
        ></textarea>
      </div>
      <div class="composer-actions">
        <input
          bind:this={fileInputEl}
          class="file-input"
          type="file"
          accept="image/png,image/jpeg,image/webp,image/gif"
          multiple
          onchange={(e) => addImages(e.currentTarget.files ?? [])}
        />
        <button
          class="attach"
          type="button"
          onclick={() => fileInputEl?.click()}
          disabled={submitting || pendingImages.length >= MAX_IMAGES}
          aria-label="Attach images"
          title="Attach images"
        >
          <span aria-hidden="true">＋</span> Image
        </button>
        {#if busy}
          <button class="cancel" onclick={onCancel}>Cancel</button>
        {/if}
        <button
          class="send"
          onclick={submit}
          disabled={submitting || (!draft.trim() && pendingImages.length === 0)}
        >{submitting ? 'Uploading…' : 'Send'}</button>
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
    transition: border-color 120ms, background 120ms;
  }

  .composer-wrap.dragging {
    border-color: var(--accent, #4f8ff7);
    background: var(--accent-soft, rgba(79, 143, 247, 0.14));
  }

  .image-previews {
    display: flex;
    gap: 0.55rem;
    padding: 0.65rem 0.75rem 0;
    overflow-x: auto;
  }

  .image-preview {
    position: relative;
    flex: 0 0 6rem;
    min-width: 0;
    padding: 0.25rem;
    border: 1px solid var(--border-strong, #3d434f);
    border-radius: var(--radius-sm, 6px);
    background: var(--bg-inset, #0d0f13);
  }

  .image-preview img {
    display: block;
    width: 100%;
    height: 3.75rem;
    object-fit: cover;
    border-radius: 4px;
  }

  .image-preview span {
    display: block;
    margin-top: 0.25rem;
    overflow: hidden;
    color: var(--text-secondary, #9aa3b2);
    font-size: 0.68rem;
    line-height: 1.2;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  button.image-remove {
    position: absolute;
    top: -0.4rem;
    right: -0.4rem;
    display: grid;
    width: 1.35rem;
    height: 1.35rem;
    padding: 0;
    place-items: center;
    border-radius: 50%;
    background: var(--bg-raised, #22262f);
    color: var(--text, #e6e9ef);
    line-height: 1;
  }

  .attachment-error {
    padding: 0.4rem 0.75rem 0;
    color: var(--danger, #f05252);
    font-size: 0.75rem;
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

  .file-input {
    display: none;
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

  button.attach {
    padding-inline: 0.65rem;
    color: var(--text-secondary, #9aa3b2);
    white-space: nowrap;
  }

  button.attach:hover:not(:disabled) {
    border-color: var(--accent-border, rgba(79, 143, 247, 0.45));
    color: var(--accent, #4f8ff7);
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
