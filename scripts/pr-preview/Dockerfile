FROM mcr.microsoft.com/playwright:v1.56.1-noble@sha256:f1e7e01021efd65dd1a2c56064be399f3e4de00fd021ac561325f2bfbb2b837a
WORKDIR /app
COPY ref/test-cases/client2-e2e/compiled-webui/toolchain/package*.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund && apt-get update && apt-get install -y --no-install-recommends ffmpeg && rm -rf /var/lib/apt/lists/*
COPY ref ./ref
COPY canonical.py record.mjs wails.js ./
USER pwuser
ENTRYPOINT ["node","record.mjs"]
