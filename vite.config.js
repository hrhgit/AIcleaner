import { defineConfig } from 'vite';

const devPort = Number(process.env.VITE_PORT || 4173);

export default defineConfig({
    server: {
        host: '127.0.0.1',
        port: devPort,
        strictPort: true,
        hmr: {
            host: '127.0.0.1',
            port: devPort,
            clientPort: devPort,
        },
        open: false,
    },
    build: {
        outDir: 'dist',
    },
});
