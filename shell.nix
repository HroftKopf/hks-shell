# Development shell for hks-shell on NixOS.
#
# The client uses the libwayland *system* backend (so it can hand wgpu the real
# wl_display / wl_surface pointers) and wgpu needs the Vulkan loader + GPU driver
# ICDs at runtime. On NixOS none of these live in standard paths, so we expose
# them via LD_LIBRARY_PATH here instead of hardcoding /nix/store paths.
#
# Usage:
#   nix-shell --run 'cargo run'
#   # or: nix-shell   then   cargo run
{ pkgs ? import <nixpkgs> { } }:

let
  runtimeLibs = with pkgs; [
    wayland # libwayland-client (dlopen'd by the system backend)
    libxkbcommon # keyboard handling in smithay-client-toolkit
    vulkan-loader # libvulkan.so.1 for the wgpu Vulkan backend
    libGL # OpenGL fallback backend
  ];
in
pkgs.mkShell {
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = runtimeLibs;

  # /run/opengl-driver/lib holds the system's Vulkan/GL driver ICDs on NixOS.
  shellHook = ''
    export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath runtimeLibs}:/run/opengl-driver/lib:$LD_LIBRARY_PATH"
  '';
}
