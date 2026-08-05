<script lang="ts">
  import Chat from './lib/Chat.svelte';
  import Stepper from './lib/Stepper.svelte';
  import Viewer from './lib/Viewer.svelte';
  import Timeline from './lib/Timeline.svelte';
  import SpecPanel from './lib/SpecPanel.svelte';
  import { connectSession, type ServerMsg, type SessionClient } from './lib/ws';

  type Message = { role: string; content: string };
  type ToolCall = { name: string; detail: string };
  type Project = { id: string; name?: string };

  let projectId = $state<string | null>(null);
  let projectChoices = $state<Project[]>([]);
  let loadingProjects = $state(false);

  let phase = $state('spec');
  let approved = $state(false);
  let conversation = $state<Message[]>([]);
  let streaming = $state('');
  let toolCalls = $state<ToolCall[]>([]);
  let iterations = $state<number[]>([]);
  let spec = $state<string | null>(null);
  let lastError = $state<string | null>(null);
  let busy = $state(false);
  let selectedParts = $state<string[]>([]);
  let libRefs = $state<string[]>([]);
  let viewing = $state<number | null>(null);
  let failedComponents = $state<string[]>([]);

  let client: SessionClient | null = null;

  function handleMessage(m: ServerMsg) {
    switch (m.type) {
      case 'snapshot':
        phase = m.phase;
        approved = m.approved;
        conversation = m.conversation;
        iterations = m.iterations;
        spec = m.spec;
        streaming = '';
        toolCalls = [];
        busy = false;
        failedComponents = [];
        break;
      case 'stream_delta':
        busy = true;
        streaming += m.text;
        break;
      case 'tool_call':
        toolCalls = [...toolCalls, { name: m.name, detail: m.detail }];
        break;
      case 'phase_state':
        phase = m.phase;
        approved = m.approved;
        break;
      case 'iteration_added':
        // The server derives `n` from the GLBs on disk and re-emits it for
        // every BuildArtifact event, including ones that produced no new
        // export (e.g. the viewer-open signal). Ignore repeats.
        if (!iterations.includes(m.n)) {
          iterations = [...iterations, m.n].sort((a, b) => a - b);
          viewing = null;
        }
        failedComponents = [];
        break;
      case 'build_progress':
        // Surfaced via tool-call-style row so it's visible in the log.
        toolCalls = [
          ...toolCalls,
          { name: `build: ${m.component}`, detail: m.status }
        ];
        if (m.status !== 'building') busy = false;
        if (m.status === 'failed') {
          if (!failedComponents.includes(m.component)) {
            failedComponents = [...failedComponents, m.component];
          }
        } else {
          failedComponents = failedComponents.filter((c) => c !== m.component);
        }
        break;
      case 'error':
        lastError = m.message;
        busy = false;
        break;
    }
  }

  function connect(id: string) {
    projectId = id;
    client = connectSession(id, {
      onMessage: handleMessage,
      onOpen: () => {
        lastError = null;
      }
    });
  }

  async function init() {
    const params = new URLSearchParams(location.search);
    const fromQuery = params.get('project');
    if (fromQuery) {
      connect(fromQuery);
      return;
    }

    loadingProjects = true;
    try {
      const res = await fetch('/api/projects');
      if (!res.ok) throw new Error(`GET /api/projects failed: ${res.status}`);
      const data = (await res.json()) as Project[];
      if (data.length === 1) {
        connect(data[0].id);
      } else {
        projectChoices = data;
      }
    } catch (e) {
      lastError = e instanceof Error ? e.message : String(e);
    } finally {
      loadingProjects = false;
    }
  }

  init();

  let newProjectName = $state('');
  let creatingProject = $state(false);

  async function createProject() {
    const name = newProjectName.trim();
    if (!name || creatingProject) return;
    creatingProject = true;
    try {
      const res = await fetch('/api/projects', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ name, description: '' })
      });
      if (!res.ok) {
        const body = await res.text();
        throw new Error(body || `POST /api/projects failed: ${res.status}`);
      }
      const { id } = (await res.json()) as { id: string };
      connect(id);
    } catch (e) {
      lastError = e instanceof Error ? e.message : String(e);
    } finally {
      creatingProject = false;
    }
  }

  function onSend(text: string) {
    if (!client) return;
    busy = true;
    conversation = [...conversation, { role: 'user', content: text }];
    client.send({ type: 'prompt', text, part_refs: [...selectedParts], lib_refs: [...libRefs] });
    selectedParts = [];
    libRefs = [];
  }

  function onPartSelected(name: string) {
    if (!selectedParts.includes(name)) {
      selectedParts = [...selectedParts, name];
    }
  }

  function onRemovePart(name: string) {
    selectedParts = selectedParts.filter((p) => p !== name);
  }

  function onAddRef(slug: string) {
    if (!libRefs.includes(slug)) {
      libRefs = [...libRefs, slug];
    }
  }

  function onRemoveRef(slug: string) {
    libRefs = libRefs.filter((r) => r !== slug);
  }

  function onSelectIteration(n: number | null) {
    viewing = n;
  }

  function onCancel() {
    client?.send({ type: 'cancel_stream' });
  }

  function onApprove() {
    client?.send({ type: 'approve_phase' });
  }

  function onAdvance() {
    client?.send({ type: 'advance' });
  }

  function onBack(target: 'spec' | 'build') {
    client?.send({ type: 'go_back', target });
  }
</script>

<main>
  {#if lastError}
    <div class="error-banner">
      <span>{lastError}</span>
      <button onclick={() => (lastError = null)}>Dismiss</button>
    </div>
  {/if}

  {#if !projectId}
    <div class="picker">
      <div class="picker-card">
        <div class="brand">
          <div class="brand-mark">Smiðr</div>
          <p class="tagline">Describe the part. Smiðr forges it.</p>
        </div>
        {#if loadingProjects}
          <p>Loading projects...</p>
        {:else}
          {#if projectChoices.length > 0}
            <h2>Select a project</h2>
            <ul>
              {#each projectChoices as p}
                <li>
                  <button onclick={() => connect(p.id)}>{p.name ?? p.id}</button>
                </li>
              {/each}
            </ul>
          {/if}
          <form
            class="new-project"
            onsubmit={(e) => {
              e.preventDefault();
              createProject();
            }}
          >
            <input
              type="text"
              placeholder="New project name..."
              bind:value={newProjectName}
              disabled={creatingProject}
            />
            <button
              type="submit"
              class="create-btn"
              disabled={creatingProject || !newProjectName.trim()}
            >
              Create
            </button>
          </form>
        {/if}
      </div>
    </div>
  {:else}
    <header class="appbar">
      <span class="wordmark">Smiðr</span>
      <div class="appbar-stepper">
        <Stepper {phase} {approved} {onApprove} {onAdvance} {onBack} />
      </div>
    </header>
    <div class="body">
      <div class="left">
        <Viewer
          {projectId}
          {iterations}
          {viewing}
          {selectedParts}
          {failedComponents}
          onPartSelected={onPartSelected}
          onPartDeselected={onRemovePart}
        />
        <Timeline {iterations} {viewing} onSelect={onSelectIteration} />
      </div>
      <div class="right">
        <Chat
          messages={conversation}
          {streaming}
          {toolCalls}
          {busy}
          {selectedParts}
          {libRefs}
          {onSend}
          {onCancel}
          {onRemovePart}
          {onAddRef}
          {onRemoveRef}
        />
        <SpecPanel {spec} {phase} {approved} {onApprove} />
      </div>
    </div>
  {/if}
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    min-height: 0;
    background: var(--bg-app);
    color: var(--text);
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    overflow: hidden;
  }

  .left {
    flex: 0 0 70%;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .left :global(.viewer) {
    flex: 1;
    min-height: 0;
  }

  .right {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    border-left: 1px solid var(--border);
    background: var(--bg-surface);
  }

  .right :global(.chat) {
    flex: 1;
    min-height: 0;
  }

  .right :global(.spec-panel) {
    flex: 0 1 40%;
    border-top: 1px solid var(--border);
    min-height: 0;
  }

  .error-banner {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 1rem;
    background: var(--danger-soft);
    color: var(--text);
    border-bottom: 1px solid var(--danger);
  }

  .error-banner button {
    background: transparent;
    border: 1px solid var(--danger);
    color: var(--danger);
    border-radius: var(--radius-sm);
    padding: 0.2rem 0.6rem;
    cursor: pointer;
    transition: background 120ms;
  }

  .error-banner button:hover {
    background: var(--danger-soft);
  }

  .picker {
    min-height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 3rem 1.5rem;
  }

  .picker-card {
    max-width: 34rem;
    width: 100%;
  }

  .appbar {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    background: var(--bg-app);
    border-bottom: 1px solid var(--border);
    padding-left: 1rem;
  }

  .appbar-stepper {
    flex: 1;
    min-width: 0;
  }

  .appbar-stepper :global(.stepper) {
    border-bottom: none;
    background: transparent;
  }

  .wordmark {
    font-weight: 650;
    letter-spacing: 0.01em;
    color: var(--text);
    white-space: nowrap;
    flex: 0 0 auto;
  }

  .brand-mark {
    font-size: 1.75rem;
    font-weight: 650;
    letter-spacing: 0.01em;
    color: var(--text);
  }

  .tagline {
    color: var(--text-secondary);
    margin: 0.25rem 0 1.75rem;
  }

  .picker h2 {
    font-size: 1.1rem;
    font-weight: 600;
    color: var(--text);
    margin: 0 0 1rem;
  }

  .picker p {
    color: var(--text-secondary);
  }

  .picker ul {
    list-style: none;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .picker button {
    width: 100%;
    text-align: left;
    font: inherit;
    padding: 0.75rem 1rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text);
    cursor: pointer;
    transition: background 120ms, border-color 120ms;
  }

  .picker button:hover {
    background: var(--bg-raised);
    border-color: var(--border-strong);
  }

  .picker button:focus-visible {
    box-shadow: var(--focus-ring);
  }

  .new-project {
    display: flex;
    gap: 0.5rem;
    margin-top: 1.25rem;
  }

  .new-project input {
    flex: 1;
    min-width: 0;
    font: inherit;
    padding: 0.6rem 0.9rem;
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    color: var(--text);
  }

  .new-project input::placeholder {
    color: var(--text-muted);
  }

  .new-project input:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .picker button.create-btn {
    width: auto;
    flex: 0 0 auto;
    text-align: center;
    font-weight: 600;
    background: var(--accent);
    border-color: transparent;
    color: var(--accent-fg);
  }

  .picker button.create-btn:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .picker button.create-btn:disabled {
    background: var(--bg-raised);
    color: var(--text-muted);
    border-color: var(--border);
    cursor: default;
  }
</style>
