import { defineConfig } from 'vite';

const devPort = Number(process.env.VITE_PORT || 5173);
const strictPort = process.env.VITE_STRICT_PORT === 'true';

export default defineConfig({
    server: {
        host: '127.0.0.1',
        port: devPort,
        strictPort,
        hmr: {
            host: '127.0.0.1',
        },
        open: false,
    },
    build: {
        outDir: 'dist',
    },
});
