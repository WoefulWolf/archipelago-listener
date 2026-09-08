FROM oven/bun:1-alpine
WORKDIR /app

# Copy dependency files and install
COPY package.json ./
RUN bun install --production

# Copy script source
COPY index.js ./

CMD ["bun", "run", "index.js"]
