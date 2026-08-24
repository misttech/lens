# One userland, two filters, nothing else that differs.
#
# A per-command filter reads the output of the tool it filters, so a comparison
# between two of them is only about the filters if git, python and rustc are
# the same build on both sides. Pinning the base by digest is the cheapest way
# to say that and mean it.
#
# Built by bench/image/build.py, which is where the tool versions are pinned.
FROM rust:1-slim-trixie@sha256:cc0448b41c3b7b7fea44f5dc50eacba729a56db365b65b7bd5e8a82d5b3db078

RUN apt-get update \
 && apt-get install -y --no-install-recommends python3 git ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Both static, so neither filter depends on the base image's libc and neither
# can be the one that got a different one.
COPY lens /usr/local/bin/lens
COPY rtk /usr/local/bin/rtk

# The harness mounts one task directory read-only. The mount point has to exist
# on the read-only root before the guest will accept the mount.
RUN mkdir -p /mnt/task

# The guest root is read-only and /tmp is the writable mount, so everything
# written at run time is pointed there. A store on the read-only root would
# swallow every write and still exit 0.
ENV LENS_STORE=/tmp/lens/store \
    LENS_LOG_DIR=/tmp/lens/log \
    HOME=/tmp/home
WORKDIR /tmp
