<script lang="ts">
  import * as THREE from 'three';
  import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
  import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';

  let {
    projectId,
    iterations,
    viewing,
    selectedParts,
    onPartSelected,
    onPartDeselected
  }: {
    projectId: string;
    iterations: number[];
    viewing: number | null;
    selectedParts: string[];
    onPartSelected: (name: string) => void;
    onPartDeselected: (name: string) => void;
  } = $props();

  let canvasEl: HTMLCanvasElement | undefined = $state();
  let containerEl: HTMLDivElement | undefined = $state();
  let loadError = $state<string | null>(null);
  let hoverLabel = $state<string | null>(null);
  let hoverX = $state(0);
  let hoverY = $state(0);

  const HOVER_COLOR = 0x66aaff;
  const SELECT_COLOR = 0xffaa33;

  type OriginalMaterial = THREE.Material | THREE.Material[];

  let renderer: THREE.WebGLRenderer | undefined;
  let scene: THREE.Scene | undefined;
  let camera: THREE.PerspectiveCamera | undefined;
  let controls: OrbitControls | undefined;
  let modelGroup: THREE.Group | undefined;
  let rafId: number | undefined;
  let resizeObserver: ResizeObserver | undefined;
  let raycaster: THREE.Raycaster | undefined;
  let pointerNdc: THREE.Vector2 | undefined;

  const originalMaterials = new Map<THREE.Mesh, OriginalMaterial>();
  let hoveredMesh: THREE.Mesh | null = null;

  let lastLoadedIteration: number | null = null;
  let lastLoadedProject: string | null = null;
  let loadGeneration = 0;

  // Click-vs-orbit-drag discrimination. OrbitControls binds its left-button
  // rotate gesture to the same canvas, so a `click` fires at the end of every
  // rotate whose pointerup lands on the canvas. Only treat a click as a
  // selection when the pointer barely moved since pointerdown.
  const CLICK_SLOP_PX = 4;
  let pressX = 0;
  let pressY = 0;
  let pressIsCandidate = false;

  function effectiveIteration(): number | null {
    if (viewing !== null) return viewing;
    if (iterations.length === 0) return null;
    return iterations[iterations.length - 1];
  }

  function iterationUrl(n: number): string {
    const padded = String(n).padStart(3, '0');
    return `/api/artifacts/${encodeURIComponent(projectId)}/iteration_${padded}.glb`;
  }

  // Walk up from the intersected object to the nearest mesh, then return the
  // INNERMOST named node at or above that mesh (stopping at the first named
  // node encountered). trimesh always wraps component nodes in an unnamed-ish
  // "world" root, so we must not keep climbing past the component's own node.
  function findMeshRoot(obj: THREE.Object3D): { mesh: THREE.Mesh; name: string } | null {
    let cur: THREE.Object3D | null = obj;
    while (cur && !(cur instanceof THREE.Mesh)) {
      cur = cur.parent;
    }
    if (!cur) return null;
    const mesh = cur as THREE.Mesh;
    let walker: THREE.Object3D | null = mesh;
    while (walker && walker !== modelGroup) {
      if (walker.name) {
        return { mesh, name: walker.name };
      }
      walker = walker.parent;
    }
    return { mesh, name: mesh.name || 'unnamed' };
  }

  function applyTint(mesh: THREE.Mesh, color: number) {
    if (!originalMaterials.has(mesh)) {
      originalMaterials.set(mesh, mesh.material);
    }
    const orig = originalMaterials.get(mesh)!;
    const prevMat = mesh.material;
    const clone = Array.isArray(orig)
      ? orig.map((m) => cloneWithTint(m, color))
      : cloneWithTint(orig, color);
    mesh.material = clone;
    disposeIfClone(prevMat, orig);
  }

  function cloneWithTint(mat: THREE.Material, color: number): THREE.Material {
    const clone = mat.clone();
    if (
      clone instanceof THREE.MeshStandardMaterial ||
      clone instanceof THREE.MeshPhysicalMaterial
    ) {
      clone.emissive = new THREE.Color(color);
      clone.emissiveIntensity = 0.6;
    }
    return clone;
  }

  function disposeIfClone(mat: OriginalMaterial, orig: OriginalMaterial) {
    if (mat === orig) return;
    if (Array.isArray(mat)) {
      mat.forEach((m) => m.dispose());
    } else {
      mat.dispose();
    }
  }

  function restoreMaterial(mesh: THREE.Mesh) {
    const orig = originalMaterials.get(mesh);
    if (orig) {
      const prevMat = mesh.material;
      mesh.material = orig;
      disposeIfClone(prevMat, orig);
    }
  }

  function meshesForName(name: string): THREE.Mesh[] {
    const out: THREE.Mesh[] = [];
    if (!modelGroup) return out;
    modelGroup.traverse((obj) => {
      if (obj instanceof THREE.Mesh) {
        const found = findMeshRoot(obj);
        if (found && found.name === name) out.push(obj);
      }
    });
    return out;
  }

  function refreshTint(name: string) {
    const meshes = meshesForName(name);
    for (const mesh of meshes) {
      if (selectedParts.includes(name)) {
        applyTint(mesh, SELECT_COLOR);
      } else if (hoveredMesh && findMeshRoot(hoveredMesh)?.name === name) {
        applyTint(mesh, HOVER_COLOR);
      } else {
        restoreMaterial(mesh);
      }
    }
  }

  function syncAllTints() {
    if (!modelGroup) return;
    const seen = new Set<string>();
    modelGroup.traverse((obj) => {
      if (obj instanceof THREE.Mesh) {
        const found = findMeshRoot(obj);
        if (found && !seen.has(found.name)) {
          seen.add(found.name);
          refreshTint(found.name);
        }
      }
    });
  }

  function clearHover() {
    if (hoveredMesh) {
      const found = findMeshRoot(hoveredMesh);
      hoveredMesh = null;
      hoverLabel = null;
      if (found) refreshTint(found.name);
    }
  }

  function onPointerMove(e: PointerEvent) {
    if (!renderer || !camera || !modelGroup || !raycaster || !pointerNdc) return;
    const rect = renderer.domElement.getBoundingClientRect();
    pointerNdc.x = ((e.clientX - rect.left) / rect.width) * 2 - 1;
    pointerNdc.y = -((e.clientY - rect.top) / rect.height) * 2 + 1;
    raycaster.setFromCamera(pointerNdc, camera);
    const hits = raycaster.intersectObjects(modelGroup.children, true);

    if (hits.length === 0) {
      clearHover();
      return;
    }

    const found = findMeshRoot(hits[0].object);
    if (!found) {
      clearHover();
      return;
    }

    if (hoveredMesh !== found.mesh) {
      clearHover();
      hoveredMesh = found.mesh;
      hoverLabel = found.name;
      refreshTint(found.name);
    }
    hoverX = e.clientX;
    hoverY = e.clientY;
  }

  function onPointerLeave() {
    clearHover();
  }

  function onPointerDown(e: PointerEvent) {
    // Left button only; anything else (orbit pan / context menu) is not a pick.
    pressIsCandidate = e.button === 0;
    pressX = e.clientX;
    pressY = e.clientY;
  }

  function onClick(e: MouseEvent) {
    if (!pressIsCandidate) return;
    pressIsCandidate = false;
    if (e.button !== 0) return;
    if (
      Math.abs(e.clientX - pressX) > CLICK_SLOP_PX ||
      Math.abs(e.clientY - pressY) > CLICK_SLOP_PX
    ) {
      return; // this was an orbit drag, not a selection
    }
    if (!renderer || !camera || !modelGroup || !raycaster || !pointerNdc) return;
    const rect = renderer.domElement.getBoundingClientRect();
    pointerNdc.x = ((e.clientX - rect.left) / rect.width) * 2 - 1;
    pointerNdc.y = -((e.clientY - rect.top) / rect.height) * 2 + 1;
    raycaster.setFromCamera(pointerNdc, camera);
    const hits = raycaster.intersectObjects(modelGroup.children, true);
    if (hits.length === 0) return;
    const found = findMeshRoot(hits[0].object);
    if (!found) return;

    if (selectedParts.includes(found.name)) {
      onPartDeselected(found.name);
    } else {
      onPartSelected(found.name);
    }
    // Actual tint is reconciled reactively once `selectedParts` updates.
  }

  function disposeModelGroup(group: THREE.Group) {
    const matsToDispose = new Set<THREE.Material>();
    group.traverse((obj) => {
      if (obj instanceof THREE.Mesh) {
        obj.geometry.dispose();
        const mat = obj.material;
        if (Array.isArray(mat)) {
          mat.forEach((m) => matsToDispose.add(m));
        } else {
          matsToDispose.add(mat);
        }
        const orig = originalMaterials.get(obj);
        if (orig) {
          if (Array.isArray(orig)) {
            orig.forEach((m) => matsToDispose.add(m));
          } else {
            matsToDispose.add(orig);
          }
        }
      }
    });
    matsToDispose.forEach((m) => m.dispose());
  }

  function frameCameraToModel() {
    if (!modelGroup || !camera || !controls) return;
    const box = new THREE.Box3().setFromObject(modelGroup);
    if (box.isEmpty()) return;
    const sphere = box.getBoundingSphere(new THREE.Sphere());
    const radius = Math.max(sphere.radius, 0.001);
    const dist = radius * 2.5;
    camera.near = Math.max(radius / 100, 0.001);
    camera.far = dist * 4 + radius * 10;
    camera.updateProjectionMatrix();
    const dir = new THREE.Vector3(1, 1, 1).normalize().multiplyScalar(dist);
    camera.position.copy(sphere.center).add(dir);
    controls.target.copy(sphere.center);
    controls.update();
  }

  async function loadIteration(n: number) {
    if (!scene) return;
    const gen = ++loadGeneration;
    const loader = new GLTFLoader();
    try {
      const gltf = await loader.loadAsync(iterationUrl(n));
      if (gen !== loadGeneration) return; // superseded by a newer request

      clearHover();
      const previous = modelGroup;
      modelGroup = gltf.scene;
      scene.add(modelGroup);
      if (previous) {
        scene.remove(previous);
        // Dispose BEFORE clearing the map: disposeModelGroup reads
        // originalMaterials to find stashed originals behind any tint clone.
        disposeModelGroup(previous);
      }
      originalMaterials.clear();
      loadError = null;
      lastLoadedIteration = n;
      lastLoadedProject = projectId;
      frameCameraToModel();
      syncAllTints();
    } catch (err) {
      if (gen !== loadGeneration) return;
      loadError = err instanceof Error ? err.message : String(err);
      // Leave the previous model (if any) on screen; do not advance
      // lastLoadedIteration so a retry of the same n is not a no-op.
    }
  }

  $effect(() => {
    if (!containerEl || !canvasEl) return;

    scene = new THREE.Scene();
    scene.background = new THREE.Color(0x1a1b1e);

    camera = new THREE.PerspectiveCamera(50, 1, 0.01, 1000);
    camera.position.set(3, 3, 3);

    renderer = new THREE.WebGLRenderer({ canvas: canvasEl, antialias: true });
    renderer.setPixelRatio(window.devicePixelRatio);

    const hemi = new THREE.HemisphereLight(0xffffff, 0x444444, 1.2);
    scene.add(hemi);
    const dir = new THREE.DirectionalLight(0xffffff, 1.0);
    dir.position.set(5, 10, 7.5);
    scene.add(dir);

    controls = new OrbitControls(camera, renderer.domElement);
    controls.enableDamping = true;

    raycaster = new THREE.Raycaster();
    pointerNdc = new THREE.Vector2();

    function resize() {
      if (!containerEl || !renderer || !camera) return;
      const w = containerEl.clientWidth;
      const h = containerEl.clientHeight;
      if (w === 0 || h === 0) return;
      renderer.setSize(w, h);
      camera.aspect = w / h;
      camera.updateProjectionMatrix();
    }

    resizeObserver = new ResizeObserver(resize);
    resizeObserver.observe(containerEl);
    resize();

    function animate() {
      rafId = requestAnimationFrame(animate);
      controls?.update();
      if (renderer && scene && camera) renderer.render(scene, camera);
    }
    animate();

    const canvas = canvasEl;
    canvas.addEventListener('pointermove', onPointerMove);
    canvas.addEventListener('pointerleave', onPointerLeave);
    canvas.addEventListener('pointerdown', onPointerDown);
    canvas.addEventListener('click', onClick);

    return () => {
      canvas.removeEventListener('pointermove', onPointerMove);
      canvas.removeEventListener('pointerleave', onPointerLeave);
      canvas.removeEventListener('pointerdown', onPointerDown);
      canvas.removeEventListener('click', onClick);
      if (rafId !== undefined) cancelAnimationFrame(rafId);
      resizeObserver?.disconnect();
      controls?.dispose();
      if (modelGroup) disposeModelGroup(modelGroup);
      originalMaterials.clear();
      renderer?.dispose();
      renderer = undefined;
      scene = undefined;
      camera = undefined;
      controls = undefined;
      modelGroup = undefined;
      hoveredMesh = null;
      hoverLabel = null;
      lastLoadedIteration = null;
      lastLoadedProject = null;
      pressIsCandidate = false;
      loadGeneration = 0;
    };
  });

  $effect(() => {
    const n = effectiveIteration();
    // Re-run when iterations/viewing/projectId change.
    void iterations;
    void viewing;
    const project = projectId;
    if (project !== lastLoadedProject) {
      // Different project: the currently shown GLB is stale regardless of n.
      lastLoadedIteration = null;
    }
    if (n === null) {
      lastLoadedIteration = null;
      return;
    }
    if (!scene) return;
    if (n === lastLoadedIteration && project === lastLoadedProject) return;
    loadIteration(n);
  });

  // Reconcile mesh tints whenever the parent's selection truth changes
  // (including being cleared after a prompt is sent).
  $effect(() => {
    void selectedParts;
    syncAllTints();
  });
</script>

<div class="viewer" bind:this={containerEl}>
  <canvas bind:this={canvasEl}></canvas>

  {#if iterations.length === 0}
    <div class="placeholder">No model yet</div>
  {/if}

  {#if loadError}
    <div class="load-error">Failed to load model: {loadError}</div>
  {/if}

  {#if hoverLabel}
    <div class="hover-label" style="left: {hoverX + 12}px; top: {hoverY + 12}px;">
      {hoverLabel}
    </div>
  {/if}
</div>

<style>
  .viewer {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    background: var(--viewer-bg, #1a1b1e);
  }

  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }

  .placeholder {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted-fg, #888);
    font-size: 0.95rem;
    pointer-events: none;
  }

  .load-error {
    position: absolute;
    top: 0.5rem;
    left: 0.5rem;
    right: 0.5rem;
    background: var(--danger, #a83232);
    color: #fff;
    padding: 0.4rem 0.6rem;
    border-radius: 0.3rem;
    font-size: 0.8rem;
    pointer-events: none;
  }

  .hover-label {
    position: fixed;
    background: rgba(0, 0, 0, 0.75);
    color: #fff;
    padding: 0.15rem 0.45rem;
    border-radius: 0.25rem;
    font-size: 0.75rem;
    pointer-events: none;
    z-index: 10;
  }
</style>
