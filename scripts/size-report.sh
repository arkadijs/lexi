#!/usr/bin/env bash
# Size of the Android library on the build host (N1: stripped .so < 4 MB): ships the working tree, builds it twice with
# cargo-ndk (stripped = what ships, unstripped = for the per-crate breakdown) into a separate target dir, and
# prints both sizes plus the biggest crates / C libraries by symbol size.
#   scripts/size-report.sh
set -euo pipefail
SRC=/tmp/lexi-android-src
cd "$(dirname "$0")/.."
. scripts/env.sh
lexi_need_host
tar --exclude=.git --exclude=target --exclude=.envrc --exclude='.env*' --exclude=dist \
    --exclude=android/.gradle --exclude=android/build --exclude=android/app/build \
    --exclude=android/app/src/main/jniLibs -cf - . | ssh "$HOST" "mkdir -p $SRC && tar -C $SRC -xf -"
OUT=$(mktemp -d)
ssh "$HOST" "docker run --rm -e CARGO_BUILD_JOBS=\$(nproc) -v $SRC:/work -v lexi-target:/work/target -w /work lexi-android-builder bash -c '
  set -e
  NM=\$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm
  CARGO_TARGET_DIR=/work/target/size-strip cargo ndk -t arm64-v8a --platform 31 build -p lexi-core --release 2>&1 | tail -1
  S=target/size-strip/aarch64-linux-android/release/liblexi_core.so
  echo STRIPPED_BYTES=\$(stat -c %s \$S)
  CARGO_PROFILE_RELEASE_STRIP=none CARGO_TARGET_DIR=/work/target/size cargo ndk -t arm64-v8a --platform 31 build -p lexi-core --release 2>&1 | tail -1
  \$NM -C --size-sort -S target/size/aarch64-linux-android/release/liblexi_core.so > target/size/syms.txt
'" | tee "$OUT/log"
ssh "$HOST" 'docker run --rm -v lexi-target:/t alpine cat /t/size/syms.txt' > "$OUT/syms.txt"
python3 - "$OUT/syms.txt" <<'P'
import re,sys,collections
tot=collections.Counter(); c=collections.Counter()
for line in open(sys.argv[1],errors="replace"):
    parts=line.rstrip("\n").split(" ",3)
    if len(parts)<4: continue
    try: size=int(parts[1],16)
    except ValueError: continue
    name=parts[3]
    m=re.match(r"^<?(?:&?(?:mut )?)?(?:dyn )?([A-Za-z_][A-Za-z0-9_]*)::",name)
    tot[m.group(1) if m else "[C] "+re.sub(r"[^A-Za-z_].*","",name.lstrip("<"))[:14]]+=size
print("symbol bytes by crate / C prefix (KB):")
for k,v in tot.most_common(16): print(f"  {v/1024:6.0f}  {k}")
P
