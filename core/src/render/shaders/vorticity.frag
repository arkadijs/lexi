#version 300 es
precision highp float;
// Vorticity confinement: pushes energy back into the small swirls the grid would otherwise smear out.
// The bot's voice raises u_curl so the flow gets livelier in proportion to what it says.
uniform sampler2D u_vel;
uniform sampler2D u_curl;
uniform vec2 u_texel;
uniform float u_strength;
uniform float u_dt;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy * u_texel;
    float L = texture(u_curl, uv - vec2(u_texel.x, 0.0)).x;
    float R = texture(u_curl, uv + vec2(u_texel.x, 0.0)).x;
    float T = texture(u_curl, uv + vec2(0.0, u_texel.y)).x;
    float B = texture(u_curl, uv - vec2(0.0, u_texel.y)).x;
    float C = texture(u_curl, uv).x;
    vec2 force = 0.5 * vec2(abs(T) - abs(B), abs(R) - abs(L));
    force /= length(force) + 0.0001;
    force *= u_strength * C;
    force.y *= -1.0;
    vec2 v = texture(u_vel, uv).xy + force * u_dt;
    o = vec4(clamp(v, -1000.0, 1000.0), 0.0, 1.0);
}
