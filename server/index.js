/**
 * server/index.js
 * Express 后端入口 — 挂载路由、CORS、静态资源
 */
import express from 'express';
import cors from 'cors';
import path from 'path';
import { fileURLToPath } from 'url';
import dotenv from 'dotenv';
import { scanRouter } from './routes/scan.js';
import { settingsRouter } from './routes/settings.js';
import { filesRouter } from './routes/files.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// 加载项目根目录下的 .env 文件
dotenv.config({ path: path.join(__dirname, '../.env') });

const app = express();
const PORT = 3001;

app.use(cors());
app.use(express.json());

app.use('/api/scan', scanRouter);
app.use('/api/settings', settingsRouter);
app.use('/api/files', filesRouter);

// 托管构建后的 Vite 前端资源
app.use(express.static(path.join(__dirname, '../dist')));

// SPA 路由回退机制：不是 API 的请求全部返回 index.html
app.get('*', (req, res) => {
    res.sendFile(path.join(__dirname, '../dist/index.html'));
});

const server = app.listen(PORT, () => {
    console.log(`[AIcleaner Server] Running on http://localhost:${PORT}`);
});

server.on('error', (err) => {
    if (err.code === 'EADDRINUSE') {
        console.warn(`[AIcleaner Server] Port ${PORT} is in use, retrying in 1s...`);
        setTimeout(() => {
            server.close();
            server.listen(PORT);
        }, 1000);
    } else {
        console.error('[AIcleaner Server] Fatal error:', err);
        process.exit(1);
    }
});
