# Oldest supported Linux baseline: everything built here runs on glibc >= 2.35.
FROM docker.io/library/ubuntu:22.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential ca-certificates cmake curl file git patchelf pkg-config xz-utils \
      desktop-file-utils libwebkit2gtk-4.1-dev libgtk-3-dev libappindicator3-dev librsvg2-dev \
      libasound2-dev libssl-dev \
      gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-pulseaudio \
      gstreamer1.0-alsa gstreamer1.0-tools m4 meson ninja-build libsndfile1-dev libdbus-1-dev \
    && rm -rf /var/lib/apt/lists/*
COPY build_libpulse.sh /usr/local/bin/
RUN build_libpulse.sh /opt/libpulse
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/opt/node/bin:/opt/uv:/usr/bin:/bin
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.98.0 --no-modify-path
RUN mkdir -p /opt/node && curl -fsSL https://nodejs.org/dist/v24.11.1/node-v24.11.1-linux-x64.tar.xz \
    | tar -xJ -C /opt/node --strip-components=1 && npm install -g pnpm@11.10.0
RUN mkdir -p /opt/uv && curl -fsSL https://astral.sh/uv/install.sh | env UV_UNMANAGED_INSTALL=/opt/uv sh
RUN chmod -R a+rX /opt
RUN apt-get update && apt-get install -y --no-install-recommends xdg-utils && rm -rf /var/lib/apt/lists/*
