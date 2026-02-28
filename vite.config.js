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
        open: `http://127.0.0.1:${devPort}/`,
        proxy: {
            '/api': {
                target: 'http://localhost:3001',
                changeOrigin: true,
            },
        },
    },
    build: {
        outDir: 'dist',
    },
});
