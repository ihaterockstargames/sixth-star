//! What the native ray-tracing prototypes share, whatever the API: the
//! synthetic scenes and rays, and the independent fp64 CPU reference that
//! checks every GPU hit. Independently authored; no game data.
#![allow(dead_code)]

use reference::V3;

/// The CPU reference: fp64, brute force, no acceleration structure, so it
/// shares no code or data structure with the GPU path.
pub mod reference {
    pub type V3 = [f64; 3];

    pub fn sub(a: V3, b: V3) -> V3 {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    pub fn cross(a: V3, b: V3) -> V3 {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }
    pub fn dot(a: V3, b: V3) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    pub fn len(a: V3) -> f64 {
        dot(a, a).sqrt()
    }

    /// One triangle hit in fp64 (Möller–Trumbore, both faces): `t`, the
    /// barycentrics `u` (vertex 1) and `v` (vertex 2), and first-order
    /// forward-error bounds of an fp32 evaluation of the same quantities.
    #[derive(Clone, Copy, Debug)]
    pub struct Hit {
        pub t: f64,
        pub u: f64,
        pub v: f64,
        /// Bound on |t32 − t| and on |u32 − u|, |v32 − v| (see `Bounds`).
        pub dt: f64,
        pub db: f64,
    }

    /// fp32 unit roundoff.
    const EPS: f64 = 5.960_464_477_539_063e-8;
    /// Constant of the bounds: products of three terms after two cross
    /// products and a dot product accumulate at most γ_6 ≈ 6ε relative
    /// error per term (Higham, "Accuracy and Stability of Numerical
    /// Algorithms", §3.1); 8 also covers the division by `det` and its
    /// own error to first order.
    const C: f64 = 8.0;

    /// Intersects origin `o`, direction `d` with triangle (a, b, c); the
    /// hit even if outside [0, tmax] or outside the triangle (callers test
    /// with the bounds), `None` only for a parallel ray.
    ///
    /// `dp` is the position uncertainty the GPU adds before its own test:
    /// an instanced ray is transformed into object space in fp32, which
    /// moves its origin by up to γ_3·(‖R‖·|o| + |T|) ≈ 4ε(|o| + |T|) for a
    /// rotation R and translation T (Higham §3.5, matrix-vector products).
    /// A shift δp of the origin moves the plane hit by δp·|d||e1×e2|/|det|
    /// in t and by about δp·|d|(|e1|+|e2|)/|det| in the barycentrics.
    pub fn intersect(o: V3, d: V3, a: V3, b: V3, c: V3, dp: f64) -> Option<Hit> {
        let (e1, e2) = (sub(b, a), sub(c, a));
        let p = cross(d, e2);
        let det = dot(e1, p);
        if det.abs() < 1e-300 {
            return None;
        }
        let s = sub(o, a);
        let q = cross(s, e1);
        let (u, v, t) = (dot(s, p) / det, dot(d, q) / det, dot(e2, q) / det);
        let (ls, ld, l1, l2) = (len(s), len(d), len(e1), len(e2));
        let ln = len(cross(e1, e2));
        let db = (C * EPS * ls + dp) * ld * (l1 + l2) / det.abs();
        let dt = C * EPS * l1 * l2 * ls / det.abs() + dp * ld * ln / det.abs();
        Some(Hit { t, u, v, dt, db })
    }

    impl Hit {
        /// Inside the triangle and the ray's range by a clear margin.
        pub fn surely_in(&self, tmax: f64) -> bool {
            let w = 1.0 - self.u - self.v;
            self.u > self.db && self.v > self.db && w > 2.0 * self.db && self.t > self.dt && self.t < tmax - self.dt
        }
        /// Inside within the error bounds (fp32 may count it as a hit).
        pub fn maybe_in(&self, tmax: f64) -> bool {
            let w = 1.0 - self.u - self.v;
            self.u >= -self.db && self.v >= -self.db && w >= -2.0 * self.db && self.t >= -self.dt && self.t <= tmax + self.dt
        }
    }
}

/// xorshift64*: reproducible scenes and rays.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [lo, hi).
    pub fn f(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * ((self.next() >> 40) as f32 / (1u64 << 24) as f32)
    }
}

/// A triangle soup: vertices (3 per triangle) and indices 0..3n.
pub struct Mesh {
    pub v: Vec<[f32; 3]>,
    pub i: Vec<u32>,
}

/// `n` random small triangles in a cube of half-size `extent`.
pub fn soup(n: usize, extent: f32, seed: u64) -> Mesh {
    let mut r = Rng(seed);
    let size = 2.0 * extent / (n as f32).cbrt();
    let mut v = Vec::with_capacity(n * 3);
    for _ in 0..n {
        let c = [r.f(-extent, extent), r.f(-extent, extent), r.f(-extent, extent)];
        for _ in 0..3 {
            v.push([c[0] + r.f(-size, size), c[1] + r.f(-size, size), c[2] + r.f(-size, size)]);
        }
    }
    Mesh { v, i: (0..n as u32 * 3).collect() }
}

/// The same mesh with every vertex moved a little (for refits).
pub fn jitter(m: &Mesh, amount: f32, seed: u64) -> Vec<[f32; 3]> {
    let mut r = Rng(seed);
    m.v.iter().map(|p| [p[0] + r.f(-amount, amount), p[1] + r.f(-amount, amount), p[2] + r.f(-amount, amount)]).collect()
}

/// Rays as the shader reads them: origin + tmax, direction + 0.
/// `coherent`: from the plane z = −(extent+50) toward +z, slightly
/// spread; otherwise random origins and directions.
pub fn rays(n: u32, extent: f32, coherent: bool, seed: u64) -> Vec<[f32; 8]> {
    let mut r = Rng(seed);
    (0..n)
        .map(|_| {
            let (o, d) = if coherent {
                let o = [r.f(-extent, extent), r.f(-extent, extent), -(extent + 50.0)];
                let d = [r.f(-0.05, 0.05), r.f(-0.05, 0.05), 1.0];
                (o, d)
            } else {
                let o = [r.f(-1.5 * extent, 1.5 * extent), r.f(-1.5 * extent, 1.5 * extent), r.f(-1.5 * extent, 1.5 * extent)];
                let d = [r.f(-1.0, 1.0), r.f(-1.0, 1.0), r.f(-1.0, 1.0)];
                (o, d)
            };
            let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
            [o[0], o[1], o[2], 1.0e4, d[0] / l, d[1] / l, d[2] / l, 0.0]
        })
        .collect()
}

pub fn bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: plain-old-data slices viewed as bytes.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}

pub fn identity() -> [f32; 12] {
    [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
}

/// A rotation about z by `a` radians, then a translation.
pub fn placed(a: f32, t: [f32; 3]) -> [f32; 12] {
    let (s, c) = a.sin_cos();
    [c, -s, 0.0, t[0], s, c, 0.0, t[1], 0.0, 0.0, 1.0, t[2]]
}

pub fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// A world-space scene for the reference: per instance its mask and
/// world triangles (fp64).
pub struct WorldScene {
    /// Mask, world triangles, and |T| (the translation's length).
    pub instances: Vec<(u8, Vec<[V3; 3]>, f64)>,
}

pub fn world(mesh: &Mesh, transforms: &[([f32; 12], u8)]) -> WorldScene {
    let instances = transforms
        .iter()
        .map(|(m, mask)| {
            let xf = |p: [f32; 3]| -> V3 {
                let p = [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])];
                let r = |k: usize| f64::from(m[4 * k]) * p[0] + f64::from(m[4 * k + 1]) * p[1] + f64::from(m[4 * k + 2]) * p[2] + f64::from(m[4 * k + 3]);
                [r(0), r(1), r(2)]
            };
            let tris = mesh.i.as_chunks::<3>().0.iter().map(|t| [xf(mesh.v[t[0] as usize]), xf(mesh.v[t[1] as usize]), xf(mesh.v[t[2] as usize])]).collect();
            let tl = (f64::from(m[3]).powi(2) + f64::from(m[7]).powi(2) + f64::from(m[11]).powi(2)).sqrt();
            (*mask, tris, tl)
        })
        .collect();
    WorldScene { instances }
}

/// Checks GPU hits against the reference for every ray: returns
/// (rays, accepted disagreements, failures, first failure text).
pub fn validate(scene: &WorldScene, rays: &[[f32; 8]], hits: &[[u32; 4]], anyhit: bool, ray_mask: u8) -> (usize, usize, usize, String) {
    let (mut tolerated, mut failed, mut first) = (0, 0, String::new());
    for (k, (ray, gpu)) in rays.iter().zip(hits).enumerate() {
        let o = [f64::from(ray[0]), f64::from(ray[1]), f64::from(ray[2])];
        let d = [f64::from(ray[4]), f64::from(ray[5]), f64::from(ray[6])];
        let tmax = f64::from(ray[3]);
        // Reference: the closest hit that is surely in, and whether any
        // triangle is maybe in (within the bounds).
        let mut best: Option<(f64, usize, usize, reference::Hit)> = None;
        // The closest triangle that is surely hit: fp32 may skip a
        // marginal closer one, never a sure one.
        let mut sure: Option<(f64, reference::Hit)> = None;
        let mut any_maybe = false;
        const EPS32: f64 = 5.960_464_477_539_063e-8;
        let dp_of = |tl: f64| 4.0 * EPS32 * (reference::len(o) + tl);
        for (ii, (mask, tris, tl)) in scene.instances.iter().enumerate() {
            if mask & ray_mask == 0 {
                continue;
            }
            let dp = dp_of(*tl);
            for (pi, t) in tris.iter().enumerate() {
                if let Some(h) = reference::intersect(o, d, t[0], t[1], t[2], dp) {
                    if h.surely_in(tmax) && sure.as_ref().is_none_or(|b| h.t < b.0) {
                        sure = Some((h.t, h));
                    }
                    if h.maybe_in(tmax) {
                        any_maybe = true;
                        if best.as_ref().is_none_or(|b| h.t < b.0) {
                            best = Some((h.t, pi, ii, h));
                        }
                    }
                }
            }
        }
        let gpu_hit = gpu[0] != 0;
        let ok = if anyhit {
            // Any hit: a GPU hit needs some triangle maybe in; a GPU
            // miss needs none surely in.
            if gpu_hit {
                any_maybe
            } else {
                !scene.instances.iter().any(|(mask, tris, tl)| {
                    mask & ray_mask != 0
                        && tris.iter().any(|t| reference::intersect(o, d, t[0], t[1], t[2], dp_of(*tl)).is_some_and(|h| h.surely_in(tmax)))
                })
            }
        } else {
            match (gpu_hit, &best) {
                (false, _) => sure.is_none(),
                (true, None) => false,
                (true, Some(_)) => {
                    // The triangle the GPU names must be maybe in at the
                    // GPU's t, and no triangle may be surely closer.
                    let (pi, ii) = ((gpu[0] - 1) as usize, gpu[1] as usize);
                    let t32 = f64::from(f32::from_bits(gpu[2]));
                    match scene.instances.get(ii).and_then(|(_, tris, tl)| tris.get(pi).map(|t| (t, *tl))) {
                        None => false,
                        Some((t, tl)) => match reference::intersect(o, d, t[0], t[1], t[2], dp_of(tl)) {
                            None => false,
                            Some(h) => h.maybe_in(tmax) && (t32 - h.t).abs() <= h.dt && sure.as_ref().is_none_or(|s| t32 <= s.0 + s.1.dt + h.dt),
                        },
                    }
                }
            }
        };
        let exact = match (&best, gpu_hit) {
            (Some(b), true) => !anyhit && (gpu[0] - 1) as usize == b.1 && gpu[1] as usize == b.2,
            (None, false) => true,
            _ => false,
        };
        if !ok {
            failed += 1;
            if first.is_empty() {
                first = format!(
                    "ray {k}: gpu prim {} inst {} t {}, reference closest maybe {:?}, closest sure {:?}",
                    gpu[0] as i64 - 1,
                    gpu[1],
                    f32::from_bits(gpu[2]),
                    best.map(|b| (b.0, b.1, b.2)),
                    sure.map(|s| s.0)
                );
            }
        } else if !exact && !anyhit {
            tolerated += 1;
        }
    }
    (rays.len(), tolerated, failed, first)
}
