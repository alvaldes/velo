// The fog under the hero: one full-screen shader and a handful of embers.
//
// The hearth video ends in a soft mask; this canvas is fixed behind the whole
// page, so what shows through that mask — and behind everything below it — is
// the same warm air the room is full of. The light sits where the video ends
// and leaves with it as the page scrolls; the fog itself moves at half the
// speed of the content, so it reads as depth, not as wallpaper.
//
// three.js is vendored next to this file: the page still asks nobody else for
// anything. Without WebGL the CSS fog under the canvas stays as it is.

import * as THREE from './vendor/three.module.min.js';

const canvas = document.getElementById('fog-canvas');
const still = window.matchMedia('(prefers-reduced-motion: reduce)');

const FOG_FRAGMENT = /* glsl */ `
precision highp float;

uniform vec2 uRes;
uniform float uTime;
uniform float uScroll;

float hash(vec2 p) {
  p = fract(p * vec2(123.34, 456.21));
  p += dot(p, p + 45.32);
  return fract(p.x * p.y);
}

float noise(vec2 p) {
  vec2 i = floor(p);
  vec2 f = fract(p);
  vec2 u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
  return mix(
    mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
    mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x),
    u.y
  );
}

// Each octave is rotated so the layers never line up into a grid.
float fbm(vec2 p) {
  const mat2 turn = mat2(0.8, -0.6, 0.6, 0.8);
  float sum = 0.0;
  float amp = 0.5;
  for (int i = 0; i < 5; i++) {
    sum += amp * noise(p);
    p = turn * p * 2.03 + 11.7;
    amp *= 0.5;
  }
  return sum;
}

void main() {
  vec2 p = (gl_FragCoord.xy - 0.5 * uRes) / uRes.y;
  float t = uTime * 0.035;

  // The far field lags the content; the near wisps almost keep up with it.
  vec2 far = vec2(p.x, p.y - uScroll * 0.45) * 1.35;
  vec2 near = vec2(p.x * 2.1, (p.y - uScroll * 0.8) * 3.0);

  // Noise warped by noise warped by noise: billows that fold instead of slide.
  vec2 q = vec2(
    fbm(far + vec2(t, -t * 0.6)),
    fbm(far + vec2(5.2, 1.3) - vec2(t * 0.7, t))
  );
  vec2 r = vec2(
    fbm(far + 2.4 * q + vec2(1.7, 9.2) + t * 1.5),
    fbm(far + 2.4 * q + vec2(8.3, 2.8) - t * 1.2)
  );
  float body = fbm(far + 2.4 * r);
  float wisps = fbm(near + r * 1.5 + vec2(t * 3.0, 0.0));
  float density = smoothstep(0.18, 1.0, body + 0.28 * wisps);

  // The hearth: anchored where the video ends, scrolling away with it.
  vec2 light = vec2(0.0, -0.2 + uScroll);
  vec2 toLight = (p - light) * vec2(0.5, 1.0);
  float glow = exp(-dot(toLight, toLight) * 2.6);

  vec3 night = vec3(0.027, 0.035, 0.051);
  vec3 umber = vec3(0.2, 0.14, 0.095);
  vec3 slate = vec3(0.15, 0.21, 0.29);
  vec3 amber = vec3(0.9, 0.56, 0.3);

  vec3 col = mix(night, umber, density);
  col = mix(col, slate, q.y * q.y * 0.55 * (1.0 - glow));
  col += amber * glow * (0.06 + 0.34 * density + 0.2 * r.x * r.x);

  // Away from the hearth the fog keeps a glow of its own: embers of warm air
  // folded through it, so the long page below is never a flat dark plate.
  col += amber * 0.3 * density * r.x * r.x;

  // Further down the page the fog thins a little so the copy keeps its contrast.
  col *= mix(1.0, 0.85, smoothstep(0.4, 2.2, uScroll));
  col *= 1.0 - 0.38 * smoothstep(0.35, 1.05, length(p * vec2(0.62, 1.0)));

  // A dither just under one step of 8-bit colour: dark gradients band without it.
  col += (hash(gl_FragCoord.xy + fract(uTime) * 61.0) - 0.5) / 160.0;

  gl_FragColor = vec4(col, 1.0);
}
`;

const EMBER_VERTEX = /* glsl */ `
uniform vec2 uRes;
uniform float uTime;
uniform float uScroll;
uniform float uPixel;
varying float vAlpha;

void main() {
  // position is a seed, not a place: x spread, y phase, z size and speed.
  vec3 seed = position;
  float life = fract(seed.y + uTime * (0.025 + 0.045 * seed.z) + uScroll * 0.3);

  vec2 p;
  p.x = (seed.x - 0.5) * (0.5 + 1.3 * life)
      + sin(uTime * 0.5 + seed.y * 40.0) * 0.035;
  p.y = -0.62 + life * 1.3;

  float flicker = 0.6 + 0.4 * sin(uTime * (2.0 + 5.0 * seed.x) + seed.y * 90.0);
  vAlpha = sin(life * 3.14159) * flicker * smoothstep(3.0, 1.2, uScroll);

  gl_Position = vec4(p.x * uRes.y / uRes.x * 2.0, p.y * 2.0, 0.0, 1.0);
  gl_PointSize = (1.5 + 3.5 * seed.z) * uPixel;
}
`;

const EMBER_FRAGMENT = /* glsl */ `
precision highp float;
varying float vAlpha;

void main() {
  float d = length(gl_PointCoord - 0.5);
  float core = smoothstep(0.5, 0.0, d);
  gl_FragColor = vec4(vec3(1.0, 0.62, 0.3) * core * core * vAlpha, 1.0);
}
`;

const PASS_THROUGH = /* glsl */ `
void main() {
  gl_Position = vec4(position.xy, 0.0, 1.0);
}
`;

function start() {
  const renderer = new THREE.WebGLRenderer({
    canvas,
    antialias: false,
    alpha: false,
    powerPreference: 'low-power',
  });

  const uniforms = {
    uRes: { value: new THREE.Vector2(1, 1) },
    uTime: { value: 0 },
    uScroll: { value: 0 },
    uPixel: { value: 1 },
  };

  const scene = new THREE.Scene();
  const camera = new THREE.Camera();

  const fog = new THREE.Mesh(
    new THREE.PlaneGeometry(2, 2),
    new THREE.ShaderMaterial({
      uniforms,
      vertexShader: PASS_THROUGH,
      fragmentShader: FOG_FRAGMENT,
      depthTest: false,
      depthWrite: false,
    })
  );
  fog.frustumCulled = false;
  scene.add(fog);

  const EMBERS = 64;
  const seeds = new Float32Array(EMBERS * 3);
  for (let i = 0; i < seeds.length; i++) seeds[i] = Math.random();
  const emberGeometry = new THREE.BufferGeometry();
  emberGeometry.setAttribute('position', new THREE.BufferAttribute(seeds, 3));
  const embers = new THREE.Points(
    emberGeometry,
    new THREE.ShaderMaterial({
      uniforms,
      vertexShader: EMBER_VERTEX,
      fragmentShader: EMBER_FRAGMENT,
      blending: THREE.AdditiveBlending,
      transparent: true,
      depthTest: false,
      depthWrite: false,
    })
  );
  embers.frustumCulled = false;
  embers.renderOrder = 1;
  scene.add(embers);

  // Fog has no fine detail, so it is drawn at half the device's resolution
  // and stretched: a quarter of the fragments for a picture nobody can tell apart.
  function resize() {
    const ratio = Math.min(window.devicePixelRatio || 1, 2) * 0.5;
    renderer.setPixelRatio(ratio);
    renderer.setSize(window.innerWidth, window.innerHeight, false);
    uniforms.uRes.value.set(canvas.width, canvas.height);
    uniforms.uPixel.value = ratio * 2;
  }

  function draw(now) {
    uniforms.uTime.value = now / 1000;
    uniforms.uScroll.value = window.scrollY / window.innerHeight;
    embers.visible = !still.matches;
    renderer.render(scene, camera);
  }

  let frame = 0;
  function loop(now) {
    draw(now);
    frame = requestAnimationFrame(loop);
  }

  // Under reduced motion the fog holds still: one frame, redrawn only when the
  // reader scrolls or resizes, at a fixed moment in time.
  const drawStill = () => draw(40_000);

  function run() {
    cancelAnimationFrame(frame);
    window.removeEventListener('scroll', drawStill);
    if (still.matches) {
      window.addEventListener('scroll', drawStill, { passive: true });
      drawStill();
    } else if (!document.hidden) {
      frame = requestAnimationFrame(loop);
    }
  }

  window.addEventListener('resize', () => {
    resize();
    if (still.matches) drawStill();
  });
  document.addEventListener('visibilitychange', run);
  still.addEventListener('change', run);

  resize();
  run();
  document.documentElement.classList.add('fog-live');
}

try {
  start();
} catch (error) {
  // No WebGL: the CSS fog under the canvas is the whole backdrop.
  canvas.remove();
}
