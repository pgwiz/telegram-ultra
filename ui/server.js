const express = require('express');
const { createProxyMiddleware } = require('http-proxy-middleware');
const path = require('path');

require('dotenv').config({ path: path.resolve(__dirname, '../.env') });

const PORT = process.env.NODE_UI_PORT || 3000;
const API_URL = `http://${process.env.API_HOST || '127.0.0.1'}:${process.env.API_PORT || 8081}`;

const app = express();

// Proxy /api requests to Rust API server
app.use('/api', createProxyMiddleware({
    target: API_URL,
    changeOrigin: true,
    onProxyReq: (proxyReq, req) => {
        // Forward cookies
        if (req.headers.cookie) {
            proxyReq.setHeader('Cookie', req.headers.cookie);
        }
    },
    onProxyRes: (proxyRes) => {
        // Allow cookies from API
        const setCookie = proxyRes.headers['set-cookie'];
        if (setCookie) {
            proxyRes.headers['set-cookie'] = setCookie;
        }
    }
}));

// Serve static files
app.use(express.static(path.join(__dirname, 'public')));

// SPA fallback - serve login.html for unmatched routes
app.get('*', (req, res) => {
    res.sendFile(path.join(__dirname, 'public', 'login.html'));
});

app.listen(PORT, () => {
    console.log(`Hermes Dashboard UI listening on http://localhost:${PORT}`);
    console.log(`Proxying /api to ${API_URL}`);
});
