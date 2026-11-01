{
  description = "Unlight, a fast non-destructive raw photo editor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      mkProject =
        pkgs:
        let
          inherit (pkgs) lib;
          craneLib = crane.mkLib pkgs;

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              (craneLib.fileset.commonCargoSources ./.)
              (lib.fileset.fileFilter (file: file.hasExt "wgsl" || file.hasExt "h" || file.hasExt "ttf") ./.)
              (lib.fileset.maybeMissing ./crates/unlight-engine/tests/golden)
              (lib.fileset.maybeMissing ./crates/unlight-app/tests/snapshots)
            ];
          };

          # wgpu and winit dlopen these, so they have to be on the RPATH.
          runtimeLibs = with pkgs; [
            vulkan-loader
            wayland
            libxkbcommon
          ];

          nativeLibs = with pkgs; [
            libraw
            lensfun
            glib
            lcms2
            libjxl
            libavif
          ];

          commonArgs = {
            inherit src;
            pname = "unlight";
            version = "0.1.0";
            strictDeps = true;
            nativeBuildInputs = with pkgs; [
              pkg-config
              nasm
              rustPlatform.bindgenHook
            ];
            buildInputs = nativeLibs ++ runtimeLibs;
          };

          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          # Loaded at runtime rather than linked, so only builds that name it depend on it.
          onnxruntimeLibrary = "${pkgs.onnxruntime}/lib/libonnxruntime.so";

          installArgs = {
            inherit cargoArtifacts;
            doCheck = false;
            postFixup = ''
              for bin in $out/bin/*; do
                patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} "$bin"
              done
            '';
          };

          lavapipeIcd = "${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.uname.processor}.json";
        in
        {
          inherit
            craneLib
            commonArgs
            cargoArtifacts
            runtimeLibs
            nativeLibs
            lavapipeIcd
            onnxruntimeLibrary
            ;

          package = craneLib.buildPackage (
            commonArgs // installArgs // { UNLIGHT_ONNXRUNTIME = onnxruntimeLibrary; }
          );

          # The AI tools are optional: this builds everything else, and Nix refuses the result if
          # anything in it still refers to ONNX Runtime.
          packageWithoutAi = craneLib.buildPackage (
            commonArgs
            // installArgs
            // {
              pname = "unlight-without-ai";
              cargoExtraArgs = "--locked --workspace --exclude unlight-ai --no-default-features";
              disallowedReferences = [ pkgs.onnxruntime ];
            }
          );
        };
    in
    {
      packages = forAllSystems (pkgs: {
        default = (mkProject pkgs).package;
        without-ai = (mkProject pkgs).packageWithoutAi;
      });

      checks = forAllSystems (
        pkgs:
        let
          project = mkProject pkgs;
        in
        {
          build = project.package;

          without-ai = project.packageWithoutAi;

          tests = project.craneLib.cargoTest (
            project.commonArgs
            // {
              inherit (project) cargoArtifacts;
              VK_DRIVER_FILES = project.lavapipeIcd;
              LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath project.runtimeLibs;
              XDG_RUNTIME_DIR = "/tmp";
              ORT_DYLIB_PATH = project.onnxruntimeLibrary;
              # Wall-clock budgets are for a quiet machine, which a build sandbox isn't.
              UNLIGHT_BUDGET_SCALE = "10";
            }
          );

          clippy = project.craneLib.cargoClippy (
            project.commonArgs
            // {
              inherit (project) cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
        }
      );

      devShells = forAllSystems (
        pkgs:
        let
          project = mkProject pkgs;
        in
        {
          default = pkgs.mkShell {
            packages =
              (with pkgs; [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
                pkg-config
                nasm
                rustPlatform.bindgenHook
                vulkan-tools
                vulkan-validation-layers
                jq
              ])
              ++ project.nativeLibs
              ++ project.runtimeLibs;

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath project.runtimeLibs;
            UNLIGHT_LAVAPIPE_ICD = project.lavapipeIcd;
            ORT_DYLIB_PATH = project.onnxruntimeLibrary;
          };
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
