// Run production renderer/state code with real Three geometry and OrbitControls.
// Only GPU rendering and DOM hosting are replaced; native pixel/input tests cover those.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';
import { build } from 'esbuild';
import * as Three from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';

const result = await build({
  entryPoints: ['renderer.js'], bundle: true, format: 'iife', write: false,
  plugins: [{
    name: 'native-fixture',
    setup(build) {
      build.onResolve({ filter: /^three$/ }, () => ({ path: 'three', namespace: 'fixture' }));
      build.onResolve({ filter: /^three\/addons\/controls\/OrbitControls\.js$/ }, () => ({ path: 'controls', namespace: 'fixture' }));
      build.onLoad({ filter: /.*/, namespace: 'fixture' }, ({ path }) => ({
        contents: path === 'controls'
          ? 'export const OrbitControls = globalThis.fixtureControls;'
          : Object.keys(Three).map(name => `export const ${name} = globalThis.fixtureThree.${name};`).join('\n'),
      }));
    },
  }],
});
const html = await readFile('native/overlay.html', 'utf8');
const stateCode = html.match(/<script nonce="grok-overlay">([\s\S]*?)<\/script>/)[1];

function fixture(scale = 2) {
  const document = new EventTarget();
  class Element extends EventTarget {
    constructor() {
      super(); this.style = {}; this.children = []; this.ownerDocument = document;
      this.clientWidth = 1280; this.clientHeight = 800;
    }
    setAttribute() {}
    getRootNode() { return document; }
    attachShadow() { return new Element(); }
    append(child) { child.parent = this; this.children.push(child); }
    remove() { this.parent?.children.splice(this.parent.children.indexOf(this), 1); }
  }
  document.documentElement = new Element();
  document.createElement = document.createElementNS = () => new Element();
  const renderers = [], controls = [];
  class Renderer {
    constructor() {
      this.domElement = new Element(); this.disposed = false; renderers.push(this);
    }
    setPixelRatio(ratio) { this.ratio = ratio; }
    setSize(width, height) {
      this.domElement.style.width = `${width}px`; this.domElement.style.height = `${height}px`;
    }
    setClearColor(color, alpha) { this.alpha = alpha; }
    render(world, camera) { world.updateMatrixWorld(); camera.updateMatrixWorld(); this.camera = camera; }
    dispose() { this.disposed = true; }
    forceContextLoss() {}
  }
  class Controls extends OrbitControls {
    constructor(...args) { super(...args); controls.push(this); }
  }
  const context = vm.createContext({
    document, window: new EventTarget(), AbortController,
    innerWidth: 1280, innerHeight: 800, devicePixelRatio: scale,
    fixtureThree: { ...Three, WebGLRenderer: Renderer }, fixtureControls: Controls,
  });
  vm.runInContext(stateCode, context);
  vm.runInContext(result.outputFiles[0].text, context);
  return { draw: context.__grokDraw, state: context.__grokOverlay, renderers, controls, document };
}

const scene = { kind: 'model3d', parts: [{
  primitive: 'box', position: [0, 0, 0], rotation: [0, 0, 0], scale: [2, 2, 2], color: '#88aaff',
}] };
const bounds = state => Array.from(state.interactiveBounds, area => Array.from(area));

test('3D remains transparent and uses logical canvas size at fractional and integer scales', () => {
  for (const scale of [1, 1.5, 2, 2.5]) {
    const { draw, state, renderers } = fixture(scale);
    draw(scene);
    const renderer = renderers[0];
    assert.equal(renderer.domElement.style.width, '1280px');
    assert.equal(renderer.domElement.style.height, '800px');
    assert.equal(renderer.alpha, 0);
    const [x, y, width, height] = bounds(state)[0];
    assert.ok(x < 640 && x + width > 640 && y < 400 && y + height > 400);
    state.clear();
    assert.equal(renderer.disposed, true);
  }
});

test('Reset recovers real OrbitControls after the model is panned entirely off screen', () => {
  const { draw, state, controls } = fixture();
  draw(scene);
  const initial = bounds(state);
  const control = controls[0];
  const position = control.object.position.clone(), target = control.target.clone();
  const right = new Three.Vector3(1, 0, 0).applyQuaternion(control.object.quaternion).multiplyScalar(100);
  control.object.position.add(right); control.target.add(right); control.update();
  assert.deepEqual(bounds(state), []);
  assert.equal(typeof state.resetView, 'function');
  state.resetView();
  assert.ok(control.object.position.distanceTo(position) < 1e-10);
  assert.ok(control.target.distanceTo(target) < 1e-10);
  bounds(state)[0].forEach((value, i) => assert.ok(Math.abs(value - initial[0][i]) < 1e-8));
  state.clear();
  assert.equal(state.resetView, null);
  assert.equal(state.sceneKind, null);
  assert.deepEqual(bounds(state), []);
});

test('Scene replacement releases the previous renderer and removes 3D controls from annotations', () => {
  const { draw, state, renderers, document } = fixture();
  draw(scene);
  draw({ kind: 'annotations', items: [{ kind: 'text', x: 100, y: 100, text: '<script>literal</script>', color: '#ffffff' }] });
  assert.equal(renderers[0].disposed, true);
  assert.equal(state.resetView, null);
  assert.deepEqual(bounds(state), []);
  assert.equal(document.documentElement.children.length, 1);
  state.clear();
  assert.equal(document.documentElement.children.length, 0);
});
