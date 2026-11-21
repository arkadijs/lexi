# Reproducible Android build environment (JDK 17, SDK, NDK, Rust + cargo-ndk, Gradle).
# Needed because the dev box is aarch64 while Google's build tools are x86_64.
# Built and used on remote by scripts/android-remote.sh (context = repo root files it ships:
# devtools/android-builder + scripts/setup-android-sdk.sh).
FROM eclipse-temurin:17-jdk
ARG CMDLINE_TOOLS=commandlinetools-linux-15859902_latest.zip
ARG CMDLINE_SHA256=4e4c464f145a7512b57d088ac6c278c03c9eea610886b35a5e0804e74eedf583
ENV ANDROID_HOME=/opt/android-sdk \
    ANDROID_NDK_HOME=/opt/android-sdk/ndk/30.0.16248370 \
    RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/android-sdk/cmdline-tools/latest/bin:/opt/android-sdk/platform-tools:$PATH
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl unzip ca-certificates build-essential cmake ninja-build pkg-config git \
    && rm -rf /var/lib/apt/lists/*
# Android command-line tools (checksum from developer.android.com)
RUN mkdir -p $ANDROID_HOME/cmdline-tools && cd /tmp \
    && curl -fsSLO https://dl.google.com/android/repository/$CMDLINE_TOOLS \
    && echo "$CMDLINE_SHA256  $CMDLINE_TOOLS" | sha256sum -c - \
    && unzip -q $CMDLINE_TOOLS -d $ANDROID_HOME/cmdline-tools && mv $ANDROID_HOME/cmdline-tools/cmdline-tools $ANDROID_HOME/cmdline-tools/latest \
    && rm $CMDLINE_TOOLS
COPY scripts/setup-android-sdk.sh /tmp/setup-android-sdk.sh
RUN /tmp/setup-android-sdk.sh && rm /tmp/setup-android-sdk.sh
# Rust + cargo-ndk
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable -t aarch64-linux-android \
    && cargo install --locked cargo-ndk && rm -rf $CARGO_HOME/registry
RUN git config --system --add safe.directory '*'
