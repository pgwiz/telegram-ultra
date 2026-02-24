/**
 * Hermes Download Nexus - Dashboard JavaScript
 * Handles auth, API calls, task rendering, polling, and session management.
 */

// =============================================
// API Client
// =============================================

class HermesAPI {
    constructor() {
        this.token = localStorage.getItem('hermes_token');
        this.chatId = localStorage.getItem('hermes_chat_id');
    }

    headers() {
        const h = { 'Content-Type': 'application/json' };
        if (this.token) {
            h['Authorization'] = 'Bearer ' + this.token;
        }
        return h;
    }

    async get(path) {
        const resp = await fetch(path, { headers: this.headers() });
        if (resp.status === 401) {
            sessionExpired();
            return null;
        }
        return resp.json();
    }

    async delete(path) {
        const resp = await fetch(path, { method: 'DELETE', headers: this.headers() });
        if (resp.status === 401) {
            sessionExpired();
            return null;
        }
        return resp.json();
    }

    async post(path, body) {
        const resp = await fetch(path, {
            method: 'POST',
            headers: this.headers(),
            body: JSON.stringify(body),
        });
        return resp.json();
    }
}

const api = new HermesAPI();

// =============================================
// Session Management
// =============================================

let sessionTimerInterval = null;

function checkAuth() {
    const token = localStorage.getItem('hermes_token');
    if (!token) {
        window.location.href = '/login.html';
        return false;
    }
    return true;
}

function startSessionTimer() {
    const loginTime = parseInt(localStorage.getItem('hermes_login_time') || '0');
    const ttl = parseInt(localStorage.getItem('hermes_expires_in') || '600');
    const expiresAt = loginTime + (ttl * 1000);

    sessionTimerInterval = setInterval(() => {
        const remaining = Math.max(0, Math.floor((expiresAt - Date.now()) / 1000));
        const el = document.getElementById('sessionTimer');
        if (!el) return;

        if (remaining <= 0) {
            sessionExpired();
            return;
        }

        const mins = Math.floor(remaining / 60);
        const secs = remaining % 60;
        el.textContent = `Session: ${mins}:${secs.toString().padStart(2, '0')}`;

        if (remaining < 60) {
            el.classList.add('warning');
        }
    }, 1000);
}

function sessionExpired() {
    if (sessionTimerInterval) clearInterval(sessionTimerInterval);
    localStorage.removeItem('hermes_token');
    localStorage.removeItem('hermes_chat_id');
    localStorage.removeItem('hermes_expires_in');
    localStorage.removeItem('hermes_login_time');

    // Show "scroll has faded" if on a dashboard page
    const main = document.querySelector('.main-content');
    if (main) {
        main.innerHTML = `
            <div class="scroll-faded">
                <h2>Your scroll has faded</h2>
                <p>Your session has expired. Please log in again.</p>
                <a href="/login.html" class="btn btn-gold">Return to Login</a>
            </div>
        `;
    } else {
        window.location.href = '/login.html';
    }
}

async function hermesLogout() {
    try {
        await api.delete('/api/auth/logout');
    } catch (e) { /* ignore */ }
    localStorage.removeItem('hermes_token');
    localStorage.removeItem('hermes_chat_id');
    localStorage.removeItem('hermes_expires_in');
    localStorage.removeItem('hermes_login_time');
    window.location.href = '/login.html';
}

// =============================================
// Admin Detection
// =============================================

function showAdminLink() {
    const el = document.getElementById('adminLink');
    if (el) el.style.display = '';
}

async function checkAdmin() {
    try {
        const data = await api.get('/api/admin/stats');
        if (data && data.stats) {
            showAdminLink();
            return true;
        }
    } catch (e) { /* not admin */ }
    return false;
}

// =============================================
// Task Rendering
// =============================================

let currentFilter = 'all';
let allTasks = [];
let pollInterval = null;

function setTab(filter) {
    currentFilter = filter;
    document.querySelectorAll('.tab').forEach(t => {
        t.classList.toggle('active', t.dataset.filter === filter);
    });
    renderTasks();
}

function normalizeStatus(status) {
    if (status === 'web_queued') return 'queued';
    return status;
}

function renderTasks() {
    const container = document.getElementById('taskList');
    if (!container) return;

    let filtered = allTasks;
    if (currentFilter !== 'all') {
        filtered = allTasks.filter(t => normalizeStatus(t.status) === currentFilter);
    }

    // Update tab counts
    updateTabCounts();

    if (filtered.length === 0) {
        container.innerHTML = `
            <div class="empty-state">
                <h3>No tasks found</h3>
                <p>Send a YouTube link to the bot or use Quick Download above.</p>
            </div>
        `;
        return;
    }

    container.innerHTML = filtered.map(task => renderTaskCard(task)).join('');
}

function updateTabCounts() {
    const counts = { all: allTasks.length, running: 0, queued: 0, done: 0, error: 0, cancelled: 0 };
    for (const t of allTasks) {
        const s = normalizeStatus(t.status);
        if (counts[s] !== undefined) counts[s]++;
    }
    for (const [key, val] of Object.entries(counts)) {
        const el = document.getElementById('count' + key.charAt(0).toUpperCase() + key.slice(1));
        if (el) el.textContent = val > 0 ? `(${val})` : '';
    }
}

function renderTaskCard(task) {
    const shortId = task.id.substring(0, 8);
    const status = normalizeStatus(task.status);
    const badge = statusBadge(status);
    const url = task.url || '';
    const truncUrl = url.length > 60 ? url.substring(0, 57) + '...' : url;
    const label = task.label || task.task_type || 'download';

    let progressHtml = '';
    if (status === 'running') {
        const pct = task.progress || 0;
        progressHtml = `
            <div class="progress-container">
                <div class="progress-fill animated" style="width:${pct}%"></div>
                <span class="progress-text">${pct}%</span>
            </div>
        `;
    }

    let actions = '';
    if (status === 'running' || status === 'queued') {
        actions = `<button class="btn btn-danger btn-sm" onclick="cancelTask('${task.id}')">Cancel</button>`;
    } else if (status === 'error' || status === 'cancelled') {
        actions = `<button class="btn btn-gold btn-sm" onclick="retryTask('${task.id}')">Retry</button>`;
    } else if (status === 'done') {
        actions = `<button class="btn btn-sm" onclick="retryTask('${task.id}')">Re-download</button>`;
    }

    const created = task.created_at ? formatDate(task.created_at) : '';
    const errorMsg = task.error_msg ? `<div class="task-error">${escapeHtml(task.error_msg)}</div>` : '';

    return `
        <div class="task-card">
            <div style="display:flex; justify-content:space-between; align-items:center">
                <span class="task-title">${escapeHtml(label)} [${shortId}]</span>
                ${badge}
            </div>
            <div class="task-url">${escapeHtml(truncUrl)}</div>
            ${progressHtml}
            ${errorMsg}
            <div class="task-meta">
                <span>${created}</span>
                <div style="display:flex; gap:8px">${actions}</div>
            </div>
        </div>
    `;
}

function statusBadge(status) {
    const cls = 'badge-' + (status || 'queued');
    return `<span class="badge ${cls}">${status || 'unknown'}</span>`;
}

async function loadTasks() {
    const data = await api.get('/api/tasks');
    if (data && data.tasks) {
        allTasks = data.tasks;
        renderTasks();
    }
}

async function cancelTask(taskId) {
    const data = await api.delete('/api/tasks/' + taskId);
    if (data) {
        showToast(data.message || data.error || 'Done', data.error ? 'error' : 'success');
        await loadTasks();
    }
}

async function retryTask(taskId) {
    try {
        const resp = await fetch('/api/tasks/' + taskId + '/retry', {
            method: 'POST',
            headers: api.headers(),
        });
        const data = await resp.json();
        showToast(data.message || data.error || 'Done', data.error ? 'error' : 'success');
        await loadTasks();
    } catch (e) {
        showToast('Network error', 'error');
    }
}

function startPolling() {
    loadTasks();
    pollInterval = setInterval(loadTasks, 3000);
}

// =============================================
// Files Rendering
// =============================================

async function loadFiles() {
    const container = document.getElementById('fileList');
    if (!container) return;

    const data = await api.get('/api/files');
    if (!data || !data.files) return;

    if (data.files.length === 0) {
        container.innerHTML = `
            <div class="empty-state">
                <h3>No completed downloads</h3>
                <p>Files will appear here once downloads complete.</p>
            </div>
        `;
        return;
    }

    // Group by date
    const groups = {};
    for (const file of data.files) {
        const date = file.finished_at ? file.finished_at.substring(0, 10) : 'Unknown';
        if (!groups[date]) groups[date] = [];
        groups[date].push(file);
    }

    let html = '';
    for (const [date, files] of Object.entries(groups)) {
        html += `<div class="date-group-header">${date}</div>`;
        html += '<div class="card">';
        for (const f of files) {
            const name = extractFilename(f.file_path || '');
            const type = guessFileType(name);
            const typeClass = type === 'video' ? 'file-type-video' : 'file-type-audio';
            html += `
                <div class="file-item">
                    <div class="file-info">
                        <div class="file-name">${escapeHtml(name)}</div>
                        <div class="file-meta">
                            <span class="file-type ${typeClass}">${type}</span>
                            ${f.url ? ' &middot; ' + escapeHtml(f.url.substring(0, 50)) : ''}
                        </div>
                    </div>
                    <div class="file-actions">
                        <button class="btn btn-gold btn-sm" onclick="downloadFile('${f.id}')">Download</button>
                        <button class="btn btn-danger btn-sm" onclick="deleteFile('${f.id}')">Delete</button>
                    </div>
                </div>
            `;
        }
        html += '</div>';
    }

    container.innerHTML = html;
}

async function downloadFile(taskId) {
    try {
        const resp = await fetch('/api/files/' + taskId + '/download', {
            headers: { 'Authorization': 'Bearer ' + api.token },
        });
        if (resp.status === 401) {
            sessionExpired();
            return;
        }
        if (!resp.ok) {
            const err = await resp.json().catch(() => ({ error: 'Download failed' }));
            const errorMsg = err.error || 'Download failed';
            if (errorMsg.includes('not found on disk') || errorMsg.includes('No file')) {
                if (confirm('File no longer exists on server. Re-download it?')) {
                    await retryTask(taskId);
                }
            } else {
                showToast(errorMsg, 'error');
            }
            return;
        }
        // Extract filename from Content-Disposition header
        const disposition = resp.headers.get('Content-Disposition') || '';
        let filename = 'download';
        const match = disposition.match(/filename="?([^"]+)"?/);
        if (match) filename = match[1];

        const blob = await resp.blob();
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        a.remove();
        URL.revokeObjectURL(url);
    } catch (e) {
        showToast('Download error: ' + e.message, 'error');
    }
}

async function deleteFile(taskId) {
    if (!confirm('Delete this file permanently?')) return;
    const data = await api.delete('/api/files/' + taskId);
    if (data) {
        showToast(data.message || data.error || 'Done', data.error ? 'error' : 'success');
        await loadFiles();
    }
}

async function clearHistory() {
    if (!confirm('Clear all download history and delete files from server?')) return;
    const data = await api.delete('/api/files/history');
    if (data) {
        showToast(data.message || data.error || 'Done', data.error ? 'error' : 'success');
        await loadFiles();
    }
}

// =============================================
// Admin
// =============================================

async function loadAdminData() {
    // Stats
    const statsData = await api.get('/api/admin/stats');
    if (statsData && statsData.stats) {
        const s = statsData.stats;
        setElText('statUsers', s.total_users);
        setElText('statTasks', s.total_tasks);
        setElText('statRunning', s.running_tasks);
        setElText('statCompleted', s.completed_tasks);
        setElText('statFailed', s.failed_tasks);
        setElText('statQueued', s.queued_tasks);
    }

    // Users
    const usersData = await api.get('/api/admin/users');
    const tbody = document.getElementById('usersTable');
    if (usersData && usersData.users && tbody) {
        if (usersData.users.length === 0) {
            tbody.innerHTML = '<tr><td colspan="5">No users</td></tr>';
        } else {
            tbody.innerHTML = usersData.users.map(u => `
                <tr>
                    <td>${u.chat_id}</td>
                    <td>${escapeHtml(u.username || '-')}</td>
                    <td>${formatDate(u.first_seen)}</td>
                    <td>${formatDate(u.last_activity)}</td>
                    <td>${u.is_admin ? 'Yes' : 'No'}</td>
                </tr>
            `).join('');
        }
    }
}

// =============================================
// Toast Notifications
// =============================================

function showToast(message, type) {
    type = type || 'info';
    const container = document.getElementById('toasts');
    if (!container) return;

    const toast = document.createElement('div');
    toast.className = 'toast ' + type;
    toast.textContent = message;
    container.appendChild(toast);

    setTimeout(() => {
        toast.style.opacity = '0';
        setTimeout(() => toast.remove(), 300);
    }, 4000);
}

// =============================================
// Utility Functions
// =============================================

function escapeHtml(str) {
    if (!str) return '';
    return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function formatDate(dateStr) {
    if (!dateStr) return '';
    try {
        const d = new Date(dateStr + (dateStr.includes('Z') ? '' : 'Z'));
        return d.toLocaleDateString() + ' ' + d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
    } catch (e) {
        return dateStr;
    }
}

function extractFilename(path) {
    if (!path) return 'Unknown';
    const parts = path.replace(/\\/g, '/').split('/');
    return parts[parts.length - 1] || 'Unknown';
}

function guessFileType(filename) {
    const lower = (filename || '').toLowerCase();
    if (lower.match(/\.(mp4|mkv|webm|avi|mov)$/)) return 'video';
    return 'audio';
}

function setElText(id, text) {
    const el = document.getElementById(id);
    if (el) el.textContent = text;
}

// =============================================
// Page Initialization
// =============================================

function hermesInit(page) {
    if (!checkAuth()) return;

    startSessionTimer();
    checkAdmin();

    switch (page) {
        case 'dashboard':
            startPolling();
            break;
        case 'files':
            loadFiles();
            break;
        case 'admin':
            loadAdminData();
            break;
        case 'scheduler':
            // Basic placeholder
            break;
    }

    // Mobile sidebar toggle
    initMobileSidebar();
}

// =============================================
// Mobile Sidebar
// =============================================

function initMobileSidebar() {
    const sidebar = document.getElementById('sidebar');
    const overlay = document.getElementById('sidebarOverlay');
    const hamburger = document.getElementById('hamburgerBtn');
    if (!sidebar || !hamburger) return;

    hamburger.addEventListener('click', () => {
        sidebar.classList.toggle('open');
        if (overlay) overlay.classList.toggle('visible');
    });

    if (overlay) {
        overlay.addEventListener('click', () => {
            sidebar.classList.remove('open');
            overlay.classList.remove('visible');
        });
    }

    // Close sidebar when a nav link is tapped on mobile
    sidebar.querySelectorAll('a').forEach(a => {
        a.addEventListener('click', () => {
            if (window.innerWidth <= 768) {
                sidebar.classList.remove('open');
                if (overlay) overlay.classList.remove('visible');
            }
        });
    });
}
