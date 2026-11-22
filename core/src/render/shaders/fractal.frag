#version 300 es
precision highp float;
// Julia set with smooth colouring (the same loop is a Mandelbrot set with u_julia = 0).
// Calm by default; the bot's voice (u_bot, 0..1) warps the coordinates and shimmers the palette in
// proportion to its loudness. The user's voice (u_mic) only nudges the slow colour drift.
uniform vec2 u_res;      // render target size in pixels
uniform vec2 u_center;   // view centre in the complex plane
uniform float u_scale;   // complex-plane height of the view
uniform float u_time;
uniform float u_bot;
uniform float u_mic;
uniform vec2 u_c;        // Julia constant
uniform float u_julia;   // 1 = Julia, 0 = Mandelbrot
uniform int u_iter;
out vec4 o;

void main() {
    vec2 uv = (gl_FragCoord.xy - 0.5 * u_res) / u_res.y;
    // Distortion proportional to the bot's voice.
    uv += u_bot * 0.035 * vec2(sin(uv.y * 16.0 + u_time * 3.1), cos(uv.x * 13.0 - u_time * 2.7));
    vec2 p = u_center + uv * u_scale;
    vec2 z = mix(vec2(0.0), p, u_julia);
    vec2 c = mix(p, u_c, u_julia);
    float n = 0.0;
    float r2 = 0.0;
    // Closest approach of the *early* orbit to a ring: gives the interior some structure. (Later iterates all
    // converge to the same attracting point, so only the first few differ from pixel to pixel.)
    float trap = 1e9;
    for (int i = 0; i < u_iter; i++) {
        z = vec2(z.x * z.x - z.y * z.y, 2.0 * z.x * z.y) + c;
        r2 = dot(z, z);
        if (i < 9) trap = min(trap, abs(sqrt(r2) - 0.45));
        if (r2 > 256.0) break;
        n += 1.0;
    }
    vec3 col;
    if (r2 <= 256.0) {
        float phase = u_time * 0.015 + u_mic * 0.05;
        float g = exp(-trap * 5.0);
        vec3 pal = 0.5 + 0.5 * cos(6.28318 * (trap * 2.2 + phase + vec3(0.0, 0.12, 0.26)));
        col = vec3(0.012, 0.016, 0.034) + pal * g * 0.26;
        col *= 1.0 + u_bot * 0.45 * sin(trap * 22.0 - u_time * 8.0);   // the interior shimmers too
    } else {
        float mu = n + 1.0 - log2(log2(r2));            // smooth iteration count
        float v = log2(mu + 1.0);                        // ~1.5 far away .. ~9 right at the boundary
        float glow = smoothstep(0.9, 6.0, v);            // the far field fades to dark, the boundary glows
        float phase = u_time * 0.015 + u_mic * 0.05;     // slow, calm colour drift
        vec3 pal = 0.5 + 0.5 * cos(6.28318 * (v * 0.17 + phase + vec3(0.0, 0.12, 0.26)));
        col = pal * glow * 1.1 + vec3(0.010, 0.014, 0.030);
        col *= 1.0 + u_bot * 0.55 * sin(mu * 0.9 - u_time * 9.0);   // shimmer with the bot's voice
        col += u_bot * 0.10 * glow;
    }
    o = vec4(col, 1.0);
}
