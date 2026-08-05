<script lang="ts">
  import Chat from './lib/Chat.svelte';
  import Stepper from './lib/Stepper.svelte';
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
        iterations = [...iterations, m.n];
        break;
      case 'build_progress':
        // Surfaced via tool-call-style row so it's visible in the log.
        toolCalls = [
          ...toolCalls,
          { name: `build: ${m.component}`, detail: m.status }
        ];
        if (m.status !== 'building') busy = false;
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
      } else if (data.length > 1) {
        projectChoices = data;
      } else {
        lastError = 'No projects found.';
      }
    } catch (e) {
      lastError = e instanceof Error ? e.message : String(e);
    } finally {
      loadingProjects = false;
    }
  }

  init();

  function onSend(text: string) {
    if (!client) return;
    busy = true;
    conversation = [...conversation, { role: 'user', content: text }];
    client.send({ type: 'prompt', text, part_refs: [], lib_refs: [] });
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
      {#if loadingProjects}
        <p>Loading projects...</p>
      {:else if projectChoices.length > 0}
        <h2>Select a project</h2>
        <ul>
          {#each projectChoices as p}
            <li>
              <button onclick={() => connect(p.id)}>{p.name ?? p.id}</button>
            </li>
          {/each}
        </ul>
      {:else}
        <p>No project selected.</p>
      {/if}
    </div>
  {:else}
    <Stepper {phase} {approved} {onApprove} {onAdvance} {onBack} />
    <div class="body">
      <Chat messages={conversation} {streaming} {toolCalls} {busy} {onSend} {onCancel} />
    </div>
  {/if}
</main>

<style>
  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    min-height: 0;
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }

  .body :global(.chat) {
    flex: 1;
  }

  .error-banner {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem 1rem;
    background: var(--danger, #a83232);
    color: #fff;
  }

  .error-banner button {
    background: transparent;
    border: 1px solid rgba(255, 255, 255, 0.6);
    color: #fff;
    border-radius: 0.3rem;
    padding: 0.2rem 0.6rem;
    cursor: pointer;
  }

  .picker {
    padding: 2rem;
  }

  .picker ul {
    list-style: none;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .picker button {
    font: inherit;
    padding: 0.5rem 1rem;
    border-radius: 0.4rem;
    border: 1px solid var(--border, #444);
    background: var(--button-bg, #2b2d31);
    color: inherit;
    cursor: pointer;
  }
</style>
