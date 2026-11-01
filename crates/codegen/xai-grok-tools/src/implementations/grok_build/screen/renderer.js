// Trusted renderer. Scene input is validated by Rust; never interpret it as code or HTML.
// Bundle with the pinned dependencies documented in README.md. No runtime network access.
import {
  AmbientLight, Box3, BoxGeometry, ConeGeometry, CylinderGeometry,
  DirectionalLight, Group, Mesh, MeshStandardMaterial, PerspectiveCamera,
  Scene, SphereGeometry, TorusGeometry, Vector3, WebGLRenderer,
} from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';

globalThis.__grokDraw = (scene) => {
  const state = globalThis.__grokOverlay;
  state.clear();
  const host = document.createElement('div');
  host.setAttribute('aria-hidden', 'true');
  host.setAttribute('data-grok-overlay', '');
  host.style.cssText = 'all:initial!important;position:fixed!important;inset:0!important;z-index:2147483647!important;pointer-events:none!important;';
  const root = host.attachShadow({ mode: 'closed' });
  const events = new AbortController();
  const cleanup = [];
  state.host = host;
  state.ownedHosts.add(host);
  state.sceneKind = scene.kind;
  state.interactiveBounds = [];
  state.dispose = () => {
    events.abort();
    for (const dispose of cleanup) dispose();
  };
  // Install cleanup before any operation that can throw, including WebGL creation.
  try {
    document.documentElement.append(host);
    const listen = (target, event, handler, options = {}) => {
      target.addEventListener(event, handler, { ...options, signal: events.signal });
    };
    listen(window, 'keydown', event => {
      if (event.key === 'Escape') {
        state.clear();
        event.preventDefault();
        event.stopImmediatePropagation();
      }
    }, { capture: true });
    listen(window, 'pagehide', () => state.clear());
    if (scene.kind === 'annotations') {
      const ns = 'http://www.w3.org/2000/svg';
      const svg = document.createElementNS(ns, 'svg');
      svg.setAttribute('width', '100%');
      svg.setAttribute('height', '100%');
      svg.setAttribute('viewBox', `0 0 ${innerWidth} ${innerHeight}`);
      root.append(svg);
      const element = (kind, attributes) => {
        const node = document.createElementNS(ns, kind);
        for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, String(value));
        svg.append(node);
        return node;
      };
      for (const item of scene.items) {
        const common = { stroke: item.color, 'stroke-width': 3, fill: 'none' };
        if (item.kind === 'rectangle') {
          element('rect', { x: item.x, y: item.y, width: item.width, height: item.height, rx: 4, ...common });
        } else if (item.kind === 'arrow') {
          element('line', { x1: item.x, y1: item.y, x2: item.to_x, y2: item.to_y, ...common });
          const angle = Math.atan2(item.to_y - item.y, item.to_x - item.x);
          const points = [
            [item.to_x - 12 * Math.cos(angle - .45), item.to_y - 12 * Math.sin(angle - .45)],
            [item.to_x, item.to_y],
            [item.to_x - 12 * Math.cos(angle + .45), item.to_y - 12 * Math.sin(angle + .45)],
          ];
          element('polyline', { points: points.map(p => p.join(',')).join(' '), ...common });
        } else if (item.kind === 'text') {
          const text = element('text', {
            x: item.x, y: item.y, fill: item.color, 'font-size': 20,
            'font-family': 'system-ui,sans-serif', 'paint-order': 'stroke', stroke: '#000000', 'stroke-width': 3,
          });
          text.textContent = item.text;
        }
      }
      listen(window, 'resize', () => state.clear());
    } else {
      if (innerWidth < 160 || innerHeight < 120) throw new Error('Viewport too small for a 3D overlay');
      const width = innerWidth, height = innerHeight;
      // The canvas spans the output so zooming and panning have no artificial
      // panel edge. Native input regions keep the surrounding desktop clickable.
      listen(window, 'resize', () => state.clear());
      const renderer = new WebGLRenderer({ alpha: true, antialias: true });
      cleanup.push(() => { renderer.dispose(); renderer.forceContextLoss(); });
      renderer.domElement.style.cssText = 'display:block;pointer-events:auto;';
      renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
      renderer.setSize(width, height);
      renderer.setClearColor('#000000', 0);
      root.append(renderer.domElement);
      const world = new Scene();
      const model = new Group();
      world.add(model);
      for (const part of scene.parts) {
        const geometry = {
          box: () => new BoxGeometry(), sphere: () => new SphereGeometry(.5, 24, 16),
          cylinder: () => new CylinderGeometry(.5, .5, 1, 24), cone: () => new ConeGeometry(.5, 1, 24),
          torus: () => new TorusGeometry(.5, .15, 12, 32),
        }[part.primitive]();
        const material = new MeshStandardMaterial({ color: part.color, metalness: .15, roughness: .35 });
        cleanup.push(() => { geometry.dispose(); material.dispose(); });
        const mesh = new Mesh(geometry, material);
        mesh.position.fromArray(part.position);
        mesh.rotation.set(...part.rotation);
        mesh.scale.fromArray(part.scale);
        model.add(mesh);
      }
      const bounds = new Box3().setFromObject(model);
      const center = bounds.getCenter(new Vector3());
      const radius = Math.max(bounds.getSize(new Vector3()).length()/2, .1);
      const camera = new PerspectiveCamera(45, width/height, radius/1000, radius*100);
      camera.position.copy(center).add(new Vector3(1, .7, 1).normalize().multiplyScalar(radius*3.5));
      const controls = new OrbitControls(camera, renderer.domElement);
      controls.target.copy(center);
      controls.minDistance = radius*.2;
      controls.maxDistance = radius*30;
      controls.update();
      controls.saveState();
      cleanup.push(() => controls.dispose());
      world.add(new AmbientLight('#ffffff', 1.6));
      const light = new DirectionalLight('#ffffff', 3);
      light.position.copy(center).add(new Vector3(radius*3, radius*4, radius*5));
      light.target.position.copy(center);
      world.add(light, light.target);
      const corners = [];
      for (const x of [bounds.min.x, bounds.max.x]) {
        for (const y of [bounds.min.y, bounds.max.y]) {
          for (const z of [bounds.min.z, bounds.max.z]) corners.push(new Vector3(x, y, z));
        }
      }
      const point = new Vector3();
      const updateBounds = () => {
        let left = Infinity, top = Infinity, right = -Infinity, bottom = -Infinity;
        for (const corner of corners) {
          point.copy(corner).applyMatrix4(camera.matrixWorldInverse);
          // Crossing the near plane makes a corner projection unbounded. Keep
          // interaction available across the viewport in that case.
          if (point.z >= -camera.near) {
            state.interactiveBounds = [[0, 0, width, height]];
            return;
          }
          point.applyMatrix4(camera.projectionMatrix);
          const x = (point.x + 1)*width/2, y = (1 - point.y)*height/2;
          left = Math.min(left, x); right = Math.max(right, x);
          top = Math.min(top, y); bottom = Math.max(bottom, y);
        }
        // A small margin makes thin objects easy to grab. Bounds follow the
        // camera, including zoom and pan; the host preserves active drags.
        left = Math.max(0, left - 12); top = Math.max(0, top - 12);
        right = Math.min(width, right + 12); bottom = Math.min(height, bottom + 12);
        state.interactiveBounds = right > left && bottom > top
          ? [[left, top, right - left, bottom - top]] : [];
      };
      // Render on interaction, not an endless animation loop in a background tab.
      const render = () => {
        renderer.render(world, camera);
        updateBounds();
      };
      controls.addEventListener('change', render);
      state.resetView = () => controls.reset();
      cleanup.push(() => controls.removeEventListener('change', render));
      listen(renderer.domElement, 'webglcontextlost', event => { event.preventDefault(); state.clear(); });
      render();
    }
    return true;
  } catch (error) {
    state.clear();
    throw error;
  }
};
