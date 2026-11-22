#version 300 es
precision highp float;
// Dye -> colour bands. Calm by default; the bot's voice (u_bot) distorts the picture and shimmers the bands
// in proportion to its loudness.
uniform sampler2D u_dye;
uniform vec2 u_res;
uniform float u_time;   // real time: drives the bot-voice shimmer
uniform float u_drift;  // slowed time: drives the calm drift of the bands
uniform float u_bot;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / u_res;
    uv += u_bot * 0.013 * vec2(sin(uv.y * 38.0 + u_time * 6.0), cos(uv.x * 31.0 - u_time * 5.3));
    vec3 c = texture(u_dye, uv).rgb;
    float l = dot(c, vec3(0.299, 0.587, 0.114));
    // flowing wave-like bands: contour lines of the ink density that drift slowly
    float bands = 0.5 + 0.5 * cos(l * 26.0 - u_drift * 0.35);
    float lines = smoothstep(0.62, 0.97, bands);
    vec3 col = c * (0.55 + 0.45 * bands) + c * lines * 0.7;
    col += u_bot * 0.10 * vec3(0.45, 0.65, 1.0) * lines * (0.5 + 0.5 * sin(u_time * 12.0 + l * 40.0));
    col = col / (1.0 + 0.45 * col);
    vec3 bg = vec3(0.008, 0.012, 0.026) * (1.2 - uv.y * 0.6);
    o = vec4(bg + col, 1.0);
}
