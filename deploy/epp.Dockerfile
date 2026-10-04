# prequal-epp image (Gateway API Inference Extension endpoint picker). From the repository root:
#   docker build -f deploy/epp.Dockerfile -t prequal-epp .
FROM rust:1.89-bookworm AS build
# llm-d-router's chart (v0.11+) hard-codes a `/bin/sh -c "sleep 5"` preStop hook, and distroless has no shell. This
# static stand-in runs only that `sleep`, so the hook works without adding a shell to the image.
RUN printf '%s\n' \
      '#include <stdio.h>' '#include <stdlib.h>' '#include <string.h>' '#include <unistd.h>' \
      'int main(int argc, char **argv) {' \
      '  if (argc == 3 && !strcmp(argv[1], "-c") && !strncmp(argv[2], "sleep ", 6)) {' \
      '    sleep((unsigned)atoi(argv[2] + 6));' \
      '    return 0;' \
      '  }' \
      '  fputs("no shell in this image: /bin/sh only runs -c \"sleep <seconds>\"\n", stderr);' \
      '  return 127;' \
      '}' > /sleep-sh.c \
    && gcc -static -Os -o /sleep-sh /sleep-sh.c && strip /sleep-sh \
    && /sleep-sh -c "sleep 0" && ! /sleep-sh -c "echo hi" 2>/dev/null
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p prequal-epp

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /sleep-sh /bin/sh
COPY --from=build /src/target/release/prequal-epp /usr/local/bin/prequal-epp
EXPOSE 9002 9003 9090
ENTRYPOINT ["/usr/local/bin/prequal-epp"]
