<script lang="ts">
  import * as THREE from 'three';
  import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
  import { GLTFLoader } from 'three/examples/jsm/loaders/GLTFLoader.js';

  let {
    projectId,
    iterations,
    viewing,
    selectedParts,
    failedComponents,
    buildProgress = [],
    busy = false,
    partsOpen,
    onPartsOpenChange,
    onPartSelected,
    onPartDeselected,
    onStartBuild = null
  }: {
    projectId: string;
    iterations: number[];
    viewing: number | null;
    selectedParts: string[];
    failedComponents: string[];
    buildProgress?: { component: string; status: string }[];
    busy?: boolean;
    partsOpen: boolean;
    onPartsOpenChange: (v: boolean) => void;
    onPartSelected: (name: string) => void;
    onPartDeselected: (name: string) => void;
    onStartBuild?: (() => void) | null;
  } = $props();

  let buildActive = $derived(busy && buildProgress.length > 0);

  let canvasEl: HTMLCanvasElement | undefined = $state();
  let containerEl: HTMLDivElement | undefined = $state();
  let loadError = $state<string | null>(null);
  let hoverLabel = $state<string | null>(null);
  let hoverX = $state(0);
  let hoverY = $state(0);

  const HOVER_COLOR = 0x4f8ff7;
  const SELECT_COLOR = 0xffaa33;
  const DIFF_COLOR = 0xd29922;
  const FAILED_COLOR = 0xf05252;
  const GHOST_OPACITY = 0.25;

  type Manifest = {
    components: { name: string; bbox: [number[], number[]]; mesh_hash: string; component?: string }[];
    dimensions: Record<string, number>;
  };

  // --- Toolbar toggle state ---
  let ghostEnabled = $state(false);
  let measureEnabled = $state(false);
  let dimensionsEnabled = $state(false);
  let ghostNote = $state<string | null>(null);
  let changedComponents = $state<Set<string>>(new Set());
  let dimensionsData = $state<Record<string, number> | null>(null);
  let dimensionsError = $state(false);
  let dimensionsLoading = $state(false);
  let partNames = $state<string[]>([]);
  let isolatedPart = $state<string | null>(null);
  let folderTitle = $state('Open session folder');
  let folderNote = $state<string | null>(null);

  // Keyed by `${projectId}:${iteration}` so a project switch can't serve a
  // stale project's cached dimensions.
  const dimensionsCache = new Map<string, Record<string, number>>();
  let dimensionsLoadGeneration = 0;

  let ghostGroup: THREE.Group | undefined;
  let ghostLoadGeneration = 0;

  // Model bounding-sphere radius (mm), refreshed each time a model is framed.
  // Drives measure-marker size so markers are visible at real model scale.
  let modelRadius = 1;

  let measureLine: THREE.Line | undefined;
  let measureMarkers: THREE.Mesh[] = [];
  let measurePointA: THREE.Vector3 | null = null;
  let measureLabel = $state<{ x: number; y: number; text: string } | null>(null);
  // Dedup key for the last rendered label so the rAF loop doesn't reassign
  // `measureLabel` (and force a Svelte update) every frame while static.
  let measureLabelKey: string | null = null;

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

  function iterationUrl(project: string, n: number): string {
    const padded = String(n).padStart(3, '0');
    return `/api/artifacts/${encodeURIComponent(project)}/iteration_${padded}.glb`;
  }

  function manifestUrl(project: string, n: number): string {
    const padded = String(n).padStart(3, '0');
    return `/api/artifacts/${encodeURIComponent(project)}/iteration_${padded}.manifest.json`;
  }

  function previousIteration(n: number): number | null {
    let best: number | null = null;
    for (const it of iterations) {
      if (it < n && (best === null || it > best)) best = it;
    }
    return best;
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
    const sourceName = name.replace(/_\d+$/, '');
    for (const mesh of meshes) {
      if (failedComponents.includes(name) || failedComponents.includes(sourceName)) {
        applyTint(mesh, FAILED_COLOR);
      } else if (selectedParts.includes(name)) {
        applyTint(mesh, SELECT_COLOR);
      } else if (changedComponents.has(name)) {
        applyTint(mesh, DIFF_COLOR);
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

    if (measureEnabled) {
      handleMeasureClick(hits[0].point);
      return;
    }

    const found = findMeshRoot(hits[0].object);
    if (!found) return;

    if (selectedParts.includes(found.name)) {
      onPartDeselected(found.name);
    } else {
      onPartSelected(found.name);
    }
    // Actual tint is reconciled reactively once `selectedParts` updates.
  }

  function disposeMeasure() {
    if (measureLine) {
      scene?.remove(measureLine);
      measureLine.geometry.dispose();
      (measureLine.material as THREE.Material).dispose();
      measureLine = undefined;
    }
    for (const marker of measureMarkers) {
      scene?.remove(marker);
      marker.geometry.dispose();
      (marker.material as THREE.Material).dispose();
    }
    measureMarkers = [];
    measurePointA = null;
    measureLabel = null;
    measureLabelKey = null;
  }

  function makeMeasureMarker(point: THREE.Vector3): THREE.Mesh {
    // Scale the marker to the model so it's actually visible: a fixed
    // 0.01-unit radius is imperceptible against a part with a bounding
    // radius of tens of millimetres.
    const radius = Math.min(Math.max(modelRadius * 0.01, 0.02), modelRadius * 0.2 || 0.02);
    const geo = new THREE.SphereGeometry(radius, 12, 12);
    const mat = new THREE.MeshBasicMaterial({ color: 0xffffff, depthTest: false });
    const mesh = new THREE.Mesh(geo, mat);
    mesh.position.copy(point);
    mesh.renderOrder = 999;
    return mesh;
  }

  function updateMeasureLabel() {
    if (!measureLine || !camera || !renderer || !measurePointA) return;
    const positions = measureLine.geometry.getAttribute('position');
    const a = new THREE.Vector3(positions.getX(0), positions.getY(0), positions.getZ(0));
    const b = new THREE.Vector3(positions.getX(1), positions.getY(1), positions.getZ(1));
    const dist = a.distanceTo(b);
    const mid = a.clone().add(b).multiplyScalar(0.5);
    const projected = mid.clone().project(camera);
    const rect = renderer.domElement.getBoundingClientRect();
    const x = rect.left + ((projected.x + 1) / 2) * rect.width;
    const y = rect.top + ((-projected.y + 1) / 2) * rect.height;
    // Behind the camera, project() returns mirrored NDC (z > 1); also hide
    // once the point falls outside the canvas rect entirely.
    const behind = projected.z > 1;
    const inside = x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    if (behind || !inside) {
      if (measureLabelKey !== null) {
        measureLabelKey = null;
        measureLabel = null;
      }
      return;
    }
    const rx = Math.round(x);
    const ry = Math.round(y);
    const key = `${rx},${ry},${dist.toFixed(2)}`;
    if (key === measureLabelKey) return; // camera unchanged; skip the rAF-rate write
    measureLabelKey = key;
    measureLabel = { x: rx, y: ry, text: `${dist.toFixed(2)} mm` };
  }

  function handleMeasureClick(point: THREE.Vector3) {
    if (!scene) return;
    if (measurePointA && measureLine) {
      // Third click: clear and start over.
      disposeMeasure();
      return;
    }
    if (!measurePointA) {
      measurePointA = point.clone();
      const marker = makeMeasureMarker(point);
      measureMarkers.push(marker);
      scene.add(marker);
      return;
    }
    // Second click: draw the line.
    const marker = makeMeasureMarker(point);
    measureMarkers.push(marker);
    scene.add(marker);

    const geometry = new THREE.BufferGeometry().setFromPoints([measurePointA, point]);
    const material = new THREE.LineBasicMaterial({ color: 0xffffff, depthTest: false });
    measureLine = new THREE.Line(geometry, material);
    measureLine.renderOrder = 1000;
    scene.add(measureLine);
    updateMeasureLabel();
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
    modelRadius = radius;
    const dist = radius * 2.5;
    camera.near = Math.max(radius / 100, 0.001);
    camera.far = dist * 4 + radius * 10;
    camera.updateProjectionMatrix();
    const dir = new THREE.Vector3(1, 1, 1).normalize().multiplyScalar(dist);
    camera.position.copy(sphere.center).add(dir);
    controls.target.copy(sphere.center);
    controls.update();
  }

  function disposeGhostGroup() {
    if (!ghostGroup) return;
    if (scene) scene.remove(ghostGroup);
    disposeModelGroupMaterials(ghostGroup);
    ghostGroup = undefined;
  }

  // Like disposeModelGroup, but doesn't consult originalMaterials (ghost
  // meshes never get tinted/selected, so there's nothing stashed for them).
  function disposeModelGroupMaterials(group: THREE.Group) {
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
      }
    });
    matsToDispose.forEach((m) => m.dispose());
  }

  async function fetchManifest(project: string, n: number): Promise<Manifest | null> {
    try {
      const res = await fetch(manifestUrl(project, n));
      if (!res.ok) return null;
      return (await res.json()) as Manifest;
    } catch {
      return null;
    }
  }

  async function loadGhost(project: string, n: number) {
    // Bump the generation FIRST so any in-flight load (including one that
    // would otherwise resolve after an early return below) is invalidated.
    const gen = ++ghostLoadGeneration;
    ghostNote = null;
    changedComponents = new Set();
    disposeGhostGroup();

    const p = previousIteration(n);
    if (p === null) {
      ghostNote = 'no previous iteration';
      return;
    }

    try {
      const [currentManifest, prevManifest] = await Promise.all([
        fetchManifest(project, n),
        fetchManifest(project, p)
      ]);
      if (gen !== ghostLoadGeneration) return;
      if (!currentManifest || !prevManifest) {
        ghostNote = 'diff unavailable';
        return;
      }

      const prevHashes = new Map(prevManifest.components.map((c) => [c.name, c.mesh_hash]));
      const changed = new Set<string>();
      for (const c of currentManifest.components) {
        const prevHash = prevHashes.get(c.name);
        if (prevHash === undefined || prevHash !== c.mesh_hash) {
          changed.add(c.name);
        }
      }

      const loader = new GLTFLoader();
      const gltf = await loader.loadAsync(iterationUrl(project, p));
      if (gen !== ghostLoadGeneration) return;

      const group = gltf.scene;
      group.traverse((obj) => {
        if (obj instanceof THREE.Mesh) {
          const mats = Array.isArray(obj.material) ? obj.material : [obj.material];
          const ghostMats = mats.map((m) => {
            const clone = m.clone();
            clone.transparent = true;
            clone.opacity = GHOST_OPACITY;
            clone.depthWrite = false;
            return clone;
          });
          const prevMats = obj.material;
          obj.material = Array.isArray(obj.material) ? ghostMats : ghostMats[0];
          if (Array.isArray(prevMats)) {
            prevMats.forEach((m) => m.dispose());
          } else {
            prevMats.dispose();
          }
          obj.raycast = () => {};
        }
      });

      scene?.add(group);
      ghostGroup = group;
      changedComponents = changed;
      syncAllTints();
    } catch {
      if (gen !== ghostLoadGeneration) return;
      ghostNote = 'diff unavailable';
    }
  }

  async function loadDimensions(project: string, n: number) {
    const cacheKey = `${project}:${n}`;
    if (dimensionsCache.has(cacheKey)) {
      dimensionsData = dimensionsCache.get(cacheKey)!;
      dimensionsError = false;
      dimensionsLoading = false;
      return;
    }
    const gen = ++dimensionsLoadGeneration;
    // Drop the previous iteration's values before awaiting: the card must
    // never show a stale measurement for the iteration now being viewed.
    dimensionsData = null;
    dimensionsError = false;
    dimensionsLoading = true;
    const manifest = await fetchManifest(project, n);
    if (gen !== dimensionsLoadGeneration) return; // superseded by a newer request
    dimensionsLoading = false;
    if (manifest && manifest.dimensions) {
      dimensionsCache.set(cacheKey, manifest.dimensions);
      dimensionsData = manifest.dimensions;
      dimensionsError = false;
    } else {
      // Do not negative-cache: a transient failure should not permanently
      // pin "no dimensions recorded" for this iteration.
      dimensionsData = null;
      dimensionsError = true;
    }
  }

  function applyIsolation() {
    if (!modelGroup) return;
    modelGroup.traverse((obj) => {
      if (obj instanceof THREE.Mesh) {
        const found = findMeshRoot(obj);
        obj.visible = isolatedPart === null || (found !== null && found.name === isolatedPart);
      }
    });
  }

  function setIsolatedPart(name: string | null) {
    isolatedPart = name;
    applyIsolation();
  }

  async function doOpenFolder() {
    folderNote = null;
    try {
      const res = await fetch(`/api/projects/${encodeURIComponent(projectId)}/open-folder`, {
        method: 'POST'
      });
      if (!res.ok) return;
      const data = (await res.json()) as { path: string };
      folderTitle = data.path;
    } catch {
      folderNote = 'could not open folder';
    }
  }

  async function loadIteration(project: string, n: number) {
    if (!scene) return;
    const gen = ++loadGeneration;
    const loader = new GLTFLoader();
    try {
      const gltf = await loader.loadAsync(iterationUrl(project, n));
      if (gen !== loadGeneration) return; // superseded by a newer request

      clearHover();
      disposeMeasure();
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

      {
        const names = new Set<string>();
        modelGroup.traverse((obj) => {
          if (obj instanceof THREE.Mesh) {
            const found = findMeshRoot(obj);
            if (found) names.add(found.name);
          }
        });
        partNames = Array.from(names).sort();
        isolatedPart = null;
      }

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
    scene.background = new THREE.Color(0x14161b);

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
      if (measureLine) updateMeasureLabel();
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
      disposeGhostGroup();
      disposeMeasure();
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
      ghostLoadGeneration = 0;
      dimensionsLoadGeneration = 0;
      changedComponents = new Set();
      ghostNote = null;
      dimensionsData = null;
      dimensionsError = false;
      dimensionsLoading = false;
      measureLabelKey = null;
      partNames = [];
      isolatedPart = null;
    };
  });

  const currentIteration = $derived(effectiveIteration());

  // Diff ghost: load/dispose the ghost group and recompute changed
  // components whenever the toggle or the effective iteration changes.
  $effect(() => {
    if (!ghostEnabled) {
      // Invalidate any in-flight loadGhost so it can't add its group /
      // diff tints after the toggle has already been switched off.
      ++ghostLoadGeneration;
      disposeGhostGroup();
      ghostNote = null;
      if (changedComponents.size > 0) {
        changedComponents = new Set();
        syncAllTints();
      }
      return;
    }
    const n = currentIteration;
    // Read projectId synchronously (not just inside an awaited helper) so
    // Svelte registers it as a dependency and a project switch re-runs this.
    const project = projectId;
    if (n === null) return;
    loadGhost(project, n);
  });

  // Measure mode: clear any in-progress measurement when the mode is
  // switched off.
  $effect(() => {
    if (!measureEnabled) {
      disposeMeasure();
    }
  });

  // Dimensions overlay: fetch (with per-iteration caching) whenever enabled
  // or the effective iteration changes.
  $effect(() => {
    if (!dimensionsEnabled) return;
    const n = currentIteration;
    // Read projectId synchronously so a project switch is a tracked
    // dependency, not just a value read after an await inside the helper.
    const project = projectId;
    if (n === null) {
      ++dimensionsLoadGeneration;
      dimensionsData = null;
      dimensionsError = false;
      dimensionsLoading = false;
      return;
    }
    loadDimensions(project, n);
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
    loadIteration(project, n);
  });

  // Reconcile mesh tints whenever the parent's selection truth changes
  // (including being cleared after a prompt is sent).
  $effect(() => {
    void selectedParts;
    void failedComponents;
    void changedComponents;
    syncAllTints();
  });
</script>

<div class="viewer" bind:this={containerEl}>
  <canvas bind:this={canvasEl}></canvas>

  <div class="toolbar" class:below-banner={!!loadError}>
    <div class="segmented">
      <button
        type="button"
        class:pressed={ghostEnabled}
        aria-pressed={ghostEnabled}
        onclick={() => (ghostEnabled = !ghostEnabled)}
      >
        Ghost
      </button>
      <button
        type="button"
        class:pressed={measureEnabled}
        aria-pressed={measureEnabled}
        onclick={() => (measureEnabled = !measureEnabled)}
      >
        Measure
      </button>
      <button
        type="button"
        class:pressed={dimensionsEnabled}
        aria-pressed={dimensionsEnabled}
        onclick={() => (dimensionsEnabled = !dimensionsEnabled)}
      >
        Dimensions
      </button>
      <button
        type="button"
        class:pressed={partsOpen}
        aria-pressed={partsOpen}
        onclick={() => onPartsOpenChange(!partsOpen)}
      >
        Parts
      </button>
    </div>
    <button
      type="button"
      class="icon-btn"
      title={folderTitle}
      aria-label="Open session folder"
      onclick={doOpenFolder}
    >
      📁
    </button>
    {#if ghostEnabled && ghostNote}
      <span class="toolbar-note">{ghostNote}</span>
    {/if}
    {#if folderNote}
      <span class="toolbar-note">{folderNote}</span>
    {/if}
  </div>

  {#if partsOpen}
    <div class="parts-panel" class:below-banner={!!loadError}>
      <div class="parts-title">Parts</div>
      {#if partNames.length === 0}
        <div class="dimensions-empty">no parts</div>
      {:else}
        <button
          type="button"
          class="parts-row"
          class:active={isolatedPart === null}
          onclick={() => setIsolatedPart(null)}
        >
          Show all
        </button>
        {#each partNames as name (name)}
          <button
            type="button"
            class="parts-row"
            class:active={isolatedPart === name}
            onclick={() => setIsolatedPart(name)}
          >
            {name}
          </button>
        {/each}
      {/if}
    </div>
  {/if}

  {#if dimensionsEnabled}
    <div class="dimensions-card" class:below-banner={!!loadError}>
      <div class="dimensions-title">Dimensions</div>
      {#if dimensionsLoading}
        <div class="dimensions-empty">loading…</div>
      {:else if dimensionsError || !dimensionsData || Object.keys(dimensionsData).length === 0}
        <div class="dimensions-empty">no dimensions recorded</div>
      {:else}
        {#each Object.entries(dimensionsData) as [key, value]}
          <div class="dimensions-row"><span>{key}</span><span>{value}</span></div>
        {/each}
      {/if}
    </div>
  {/if}

  {#if measureLabel}
    <div class="measure-label" style="left: {measureLabel.x}px; top: {measureLabel.y}px;">
      {measureLabel.text}
    </div>
  {/if}

  {#if iterations.length === 0}
    <div class="placeholder">
      {#if buildActive}
        <div class="build-progress-panel" role="status" aria-live="polite">
          <div class="build-progress-header">
            <span class="spinner"></span>
            <span>Forging…</span>
          </div>
          <ul class="build-progress-list">
            {#each buildProgress as p (p.component)}
              <li class="build-progress-row status-{p.status}">
                <span class="build-progress-icon">
                  {#if p.status === 'done'}✓{:else if p.status === 'failed'}✗{:else}⏳{/if}
                </span>
                <span class="build-progress-name">{p.component}</span>
              </li>
            {/each}
          </ul>
        </div>
      {:else}
        <span>No model yet</span>
        {#if onStartBuild}
          <button class="start-build" onclick={onStartBuild}>⚒ Forge the model</button>
          <span class="start-build-hint">Runs a build from the approved spec</span>
        {/if}
      {/if}
    </div>
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
    background: var(--viewer-bg, #14161b);
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
    flex-direction: column;
    gap: 0.9rem;
    align-items: center;
    justify-content: center;
    color: var(--text-muted, #6b7280);
    font-size: 0.9rem;
    pointer-events: none;
  }

  .start-build {
    pointer-events: auto;
    font: inherit;
    font-weight: 600;
    font-size: 0.95rem;
    padding: 0.65rem 1.4rem;
    background: var(--accent, #4f8ff7);
    color: var(--accent-fg, #ffffff);
    border: none;
    border-radius: var(--radius-md, 10px);
    cursor: pointer;
    transition: background 120ms;
  }

  .start-build:hover {
    background: var(--accent-hover, #6ba1f8);
  }

  .start-build:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .start-build-hint {
    font-size: 0.8rem;
    color: var(--text-muted, #6b7280);
  }

  .build-progress-panel {
    pointer-events: auto;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    min-width: 14rem;
    max-width: 22rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-2);
    padding: 0.9rem 1.1rem;
  }

  .build-progress-header {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    font-weight: 600;
    color: var(--text);
  }

  .spinner {
    width: 1rem;
    height: 1rem;
    border-radius: 50%;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    animation: spin 900ms linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .build-progress-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  .build-progress-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-family: var(--font-mono, monospace);
    font-size: 0.82rem;
  }

  .build-progress-icon {
    width: 1.1rem;
    text-align: center;
    flex: 0 0 auto;
  }

  .build-progress-row.status-building .build-progress-icon {
    color: var(--text-secondary);
  }

  .build-progress-row.status-done .build-progress-icon {
    color: var(--success);
  }

  .build-progress-row.status-failed .build-progress-icon {
    color: var(--danger);
  }

  .build-progress-name {
    color: var(--text);
  }

  .load-error {
    position: absolute;
    top: 0.5rem;
    left: 0.5rem;
    right: 0.5rem;
    background: rgba(240, 82, 82, 0.16);
    border: 1px solid var(--danger, #f05252);
    color: var(--text, #e6e9ef);
    padding: 0.4rem 0.6rem;
    border-radius: var(--radius-sm, 6px);
    font-size: 0.8rem;
    pointer-events: none;
    z-index: 6;
  }

  .toolbar {
    position: absolute;
    top: 0.6rem;
    left: 0.6rem;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    pointer-events: auto;
    z-index: 5;
  }

  .toolbar.below-banner {
    top: 3.1rem;
  }

  .segmented {
    display: inline-flex;
    background: rgba(26, 29, 36, 0.9);
    border: 1px solid var(--border-strong, #3d434f);
    border-radius: var(--radius-sm, 6px);
    overflow: hidden;
    backdrop-filter: blur(6px);
    box-shadow: var(--shadow-2, 0 8px 24px rgba(0, 0, 0, 0.45));
  }

  .segmented button {
    font: inherit;
    font-family: inherit;
    font-size: 0.75rem;
    font-weight: 500;
    padding: 0.35rem 0.7rem;
    background: transparent;
    border: none;
    border-right: 1px solid var(--border, #2e333d);
    color: var(--text-secondary, #9aa3b2);
    cursor: pointer;
    transition:
      background 120ms,
      color 120ms;
  }

  .segmented button:last-child {
    border-right: none;
  }

  .segmented button:hover {
    background: var(--bg-hover, #2a2f3a);
    color: var(--text, #e6e9ef);
  }

  .segmented button.pressed {
    background: var(--accent, #4f8ff7);
    color: var(--accent-fg, #0b1220);
    font-weight: 600;
  }

  .segmented button:focus-visible {
    outline: 2px solid rgba(79, 143, 247, 0.5);
    outline-offset: -2px;
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.9rem;
    height: 1.9rem;
    font-size: 0.95rem;
    background: rgba(26, 29, 36, 0.9);
    border: 1px solid var(--border-strong, #3d434f);
    border-radius: var(--radius-sm, 6px);
    color: var(--text-secondary, #9aa3b2);
    cursor: pointer;
    backdrop-filter: blur(6px);
    box-shadow: var(--shadow-2, 0 8px 24px rgba(0, 0, 0, 0.45));
    transition: background 120ms, color 120ms;
  }

  .icon-btn:hover {
    background: var(--bg-hover, #2a2f3a);
    color: var(--text, #e6e9ef);
  }

  .icon-btn:focus-visible {
    outline: 2px solid rgba(79, 143, 247, 0.5);
    outline-offset: -2px;
  }

  .parts-panel {
    position: absolute;
    top: 3rem;
    left: 0.6rem;
    min-width: 12rem;
    max-width: 16rem;
    max-height: 55%;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    background: rgba(26, 29, 36, 0.92);
    border: 1px solid var(--border-strong, #3d434f);
    border-radius: var(--radius-md, 10px);
    box-shadow: var(--shadow-2, 0 8px 24px rgba(0, 0, 0, 0.45));
    color: var(--text, #e6e9ef);
    padding: 0.6rem 0.5rem;
    backdrop-filter: blur(6px);
    pointer-events: auto;
    z-index: 5;
  }

  .parts-panel.below-banner {
    top: 5.5rem;
  }

  .parts-title {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-secondary, #9aa3b2);
    font-weight: 600;
    padding: 0.1rem 0.4rem 0.4rem;
  }

  .parts-row {
    text-align: left;
    font: inherit;
    font-size: 0.78rem;
    padding: 0.35rem 0.5rem;
    background: transparent;
    border: none;
    border-radius: var(--radius-sm, 6px);
    color: var(--text-secondary, #9aa3b2);
    cursor: pointer;
    transition: background 120ms, color 120ms;
  }

  .parts-row:hover {
    background: var(--bg-hover, #2a2f3a);
    color: var(--text, #e6e9ef);
  }

  .parts-row.active {
    background: var(--accent-soft);
    color: var(--text, #e6e9ef);
    font-weight: 600;
  }

  .parts-row:focus-visible {
    box-shadow: var(--focus-ring);
    outline: none;
  }

  .toolbar-note {
    font-size: 0.7rem;
    color: var(--text-secondary, #9aa3b2);
    background: rgba(13, 15, 19, 0.8);
    border: 1px solid var(--border, #2e333d);
    padding: 0.2rem 0.45rem;
    border-radius: var(--radius-sm, 6px);
  }

  .dimensions-card {
    position: absolute;
    top: 0.6rem;
    right: 0.6rem;
    max-width: 45%;
    max-height: 60%;
    overflow: auto;
    background: rgba(26, 29, 36, 0.92);
    border: 1px solid var(--border-strong, #3d434f);
    border-radius: var(--radius-md, 10px);
    box-shadow: var(--shadow-2, 0 8px 24px rgba(0, 0, 0, 0.45));
    color: var(--text, #e6e9ef);
    padding: 0.6rem 0.7rem;
    font-size: 0.75rem;
    backdrop-filter: blur(6px);
    pointer-events: none;
    z-index: 5;
  }

  .dimensions-card.below-banner {
    top: 3.1rem;
  }

  .dimensions-title {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-secondary, #9aa3b2);
    font-weight: 600;
    margin-bottom: 0.4rem;
  }

  .dimensions-empty {
    color: var(--text-muted, #6b7280);
  }

  .dimensions-row {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.15rem 0;
    font-family: var(--font-mono, monospace);
  }

  .dimensions-row span:first-child {
    color: var(--text-secondary, #9aa3b2);
  }

  .dimensions-row span:last-child {
    color: var(--text, #e6e9ef);
  }

  .measure-label {
    position: fixed;
    transform: translate(-50%, -50%);
    background: rgba(13, 15, 19, 0.9);
    border: 1px solid var(--border-strong, #3d434f);
    color: var(--text, #e6e9ef);
    border-radius: var(--radius-sm, 6px);
    padding: 0.2rem 0.5rem;
    font-size: 0.72rem;
    font-family: var(--font-mono, monospace);
    box-shadow: var(--shadow-2, 0 8px 24px rgba(0, 0, 0, 0.45));
    pointer-events: none;
    z-index: 10;
  }

  .hover-label {
    position: fixed;
    background: rgba(13, 15, 19, 0.9);
    border: 1px solid var(--border-strong, #3d434f);
    color: var(--text, #e6e9ef);
    border-radius: var(--radius-sm, 6px);
    padding: 0.2rem 0.5rem;
    font-size: 0.72rem;
    font-family: var(--font-mono, monospace);
    box-shadow: var(--shadow-2, 0 8px 24px rgba(0, 0, 0, 0.45));
    pointer-events: none;
    z-index: 10;
  }
</style>
