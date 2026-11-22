#version 300 es
precision highp float;
uniform sampler2D u_tex;
uniform vec2 u_res;
out vec4 o;
void main() { o = vec4(texture(u_tex, gl_FragCoord.xy / u_res).rgb, 1.0); }
