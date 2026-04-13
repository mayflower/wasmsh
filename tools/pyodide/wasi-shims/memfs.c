/*
 * Pure in-memory filesystem for the standalone Pyodide artifact.
 *
 * Replaces Emscripten's JS-based MEMFS with a C implementation that
 * lives entirely inside the wasm module. No WASI filesystem calls.
 * Only stdin/stdout/stderr (fd 0/1/2) use WASI fd_read/fd_write.
 *
 * The filesystem is a flat node table. Embedded files (from --embed-file)
 * are loaded as zero-copy pointers into the wasm data section.
 * Created files get malloc'd buffers.
 */
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <errno.h>
#include <sys/stat.h>
/* Include musl's internal FILE struct definition for __stdio_read/write. */
#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>

/* musl FILE flags and layout (from src/internal/stdio_impl.h).
 * The flags field is the FIRST member of the FILE struct. */
#define F_EOF  16
#define F_ERR  32
/* Access flags via cast — first field of musl FILE is unsigned flags. */
#define FILE_FLAGS(f) (*(unsigned *)(f))

/* ── Configuration ──────────────────────────────────────────── */

#define MEMFS_MAX_NODES 16384
#define MEMFS_MAX_FDS   256

/* ── Node table ─────────────────────────────────────────────── */

#define MEMFS_DIR  1
#define MEMFS_FILE 2

typedef struct {
    uint8_t  type;              /* MEMFS_DIR or MEMFS_FILE, 0 = free */
    char     path[512];
    const uint8_t *embed_data;  /* zero-copy pointer into wasm data section */
    uint8_t  *own_data;         /* malloc'd for writable files */
    uint32_t size;
    uint32_t capacity;
} MemfsNode;

static MemfsNode g_nodes[MEMFS_MAX_NODES];
static int       g_node_count = 0;

/* ── Fd table ───────────────────────────────────────────────── */

typedef struct {
    int      node;    /* index into g_nodes, -1 = unused */
    uint32_t pos;
    int      flags;   /* O_RDONLY=0, O_WRONLY=1, O_RDWR=2 */
} MemfsFd;

static MemfsFd g_fds[MEMFS_MAX_FDS];
static int     g_init = 0;

static void memfs_init(void) {
    if (g_init) return;
    g_init = 1;
    for (int i = 0; i < MEMFS_MAX_FDS; i++) g_fds[i].node = -1;
    /* Root directory always exists. */
    g_nodes[0].type = MEMFS_DIR;
    strcpy(g_nodes[0].path, "/");
    g_node_count = 1;
}

/* ── Path helpers ───────────────────────────────────────────── */

/* Normalize path: collapse multiple slashes, remove trailing slash. */
static const char *normalize_path(const char *path, char *buf, int buflen) {
    int j = 0;
    for (int i = 0; path[i] && j < buflen - 1; i++) {
        if (path[i] == '/' && j > 0 && buf[j-1] == '/') continue;
        buf[j++] = path[i];
    }
    /* Remove trailing slash (except for root "/") */
    if (j > 1 && buf[j-1] == '/') j--;
    buf[j] = '\0';
    return buf;
}

static int node_find(const char *path) {
    char norm[512];
    path = normalize_path(path, norm, sizeof(norm));
    for (int i = 0; i < g_node_count; i++)
        if (g_nodes[i].type && strcmp(g_nodes[i].path, path) == 0)
            return i;
    return -1;
}

static int node_create(const char *path, int type) {
    if (g_node_count >= MEMFS_MAX_NODES) return -1;
    int i = g_node_count++;
    g_nodes[i].type = type;
    strncpy(g_nodes[i].path, path, sizeof(g_nodes[i].path) - 1);
    g_nodes[i].path[sizeof(g_nodes[i].path) - 1] = '\0';
    g_nodes[i].embed_data = NULL;
    g_nodes[i].own_data = NULL;
    g_nodes[i].size = 0;
    g_nodes[i].capacity = 0;
    return i;
}

static int fd_alloc(void) {
    for (int i = 3; i < MEMFS_MAX_FDS; i++)
        if (g_fds[i].node == -1) return i;
    return -1;
}

/* ── Public API for _emscripten_fs_load_embedded_files ─────── */

void memfs_mkdir(const char *path) {
    memfs_init();
    if (node_find(path) >= 0) return;
    node_create(path, MEMFS_DIR);
}

void memfs_create_file(const char *path, const uint8_t *data, uint32_t len) {
    memfs_init();
    int i = node_find(path);
    if (i < 0) i = node_create(path, MEMFS_FILE);
    if (i < 0) return;
    g_nodes[i].type = MEMFS_FILE;
    g_nodes[i].embed_data = data;
    g_nodes[i].own_data = NULL;
    g_nodes[i].size = len;
    g_nodes[i].capacity = 0;
}

/* ── memfs_read / memfs_write ───────────────────────────────── */

static ssize_t memfs_read(int fd, uint8_t *buf, size_t count) {
    MemfsFd *f = &g_fds[fd];
    MemfsNode *n = &g_nodes[f->node];
    const uint8_t *src = n->embed_data ? n->embed_data : n->own_data;
    if (!src || f->pos >= n->size) return 0;
    uint32_t avail = n->size - f->pos;
    if (count > avail) count = avail;
    memcpy(buf, src + f->pos, count);
    f->pos += count;
    return (ssize_t)count;
}

static ssize_t memfs_write(int fd, const uint8_t *buf, size_t count) {
    MemfsFd *f = &g_fds[fd];
    MemfsNode *n = &g_nodes[f->node];
    uint32_t needed = f->pos + count;
    if (needed > n->capacity) {
        uint32_t cap = needed * 2;
        if (cap < 4096) cap = 4096;
        uint8_t *p = (uint8_t *)realloc(n->own_data, cap);
        if (!p) return -1;
        if (n->embed_data && !n->own_data) {
            memcpy(p, n->embed_data, n->size);
            n->embed_data = NULL;
        }
        n->own_data = p;
        n->capacity = cap;
    }
    memcpy(n->own_data + f->pos, buf, count);
    f->pos += count;
    if (f->pos > n->size) n->size = f->pos;
    return (ssize_t)count;
}

/* ── Syscall overrides (strong, override weak standalone stubs) */

int __syscall_openat(int dirfd, intptr_t path_ptr, int flags, ...) {
    (void)dirfd;
    memfs_init();
    const char *path = (const char *)path_ptr;
    if (!strcmp(path, "/dev/stdin"))  return 0;
    if (!strcmp(path, "/dev/stdout")) return 1;
    if (!strcmp(path, "/dev/stderr")) return 2;

    int idx = node_find(path);
    if (idx < 0 && (flags & 0100 /* O_CREAT */)) {
        idx = node_create(path, MEMFS_FILE);
    }
    if (idx < 0) return -44; /* -ENOENT */
    /* Reject write access to directories, but allow read-only opens
     * (needed by opendir → os.listdir → CPython's _fill_cache). */
    if (g_nodes[idx].type == MEMFS_DIR && (flags & 3) != 0)
        return -21; /* -EISDIR */

    int fd = fd_alloc();
    if (fd < 0) return -24; /* -EMFILE */
    g_fds[fd].node = idx;
    g_fds[fd].pos = 0;
    g_fds[fd].flags = flags & 3;
    if (flags & 01000 /* O_TRUNC */) {
        if (g_nodes[idx].own_data) g_nodes[idx].size = 0;
    }
    if (flags & 02000 /* O_APPEND */) {
        g_fds[fd].pos = g_nodes[idx].size;
    }
    return fd;
}

int __syscall_stat64(intptr_t path_ptr, intptr_t buf) {
    memfs_init();
    int idx = node_find((const char *)path_ptr);
    if (idx < 0) return -44;
    /* Use real struct stat to get correct layout. */
    struct stat *st = (struct stat *)buf;
    memset(st, 0, sizeof(struct stat));
    st->st_mode = (g_nodes[idx].type == MEMFS_DIR) ? 040755 : 0100644;
    st->st_size = g_nodes[idx].size;
    st->st_nlink = 1;
    return 0;
}

int __syscall_fstat64(int fd, intptr_t buf) {
    memfs_init();
    if (fd < 3 || fd >= MEMFS_MAX_FDS || g_fds[fd].node < 0) return -9;
    int idx = g_fds[fd].node;
    struct stat *st = (struct stat *)buf;
    memset(st, 0, sizeof(struct stat));
    st->st_mode = (g_nodes[idx].type == MEMFS_DIR) ? 040755 : 0100644;
    st->st_size = g_nodes[idx].size;
    st->st_nlink = 1;
    return 0;
}

int __syscall_getcwd(intptr_t buf, int size) {
    if (size < 2) return -34; /* -ERANGE */
    ((char *)buf)[0] = '/';
    ((char *)buf)[1] = '\0';
    return 0;
}

int __syscall_faccessat(int dirfd, intptr_t path_ptr, int mode, int flags) {
    (void)dirfd; (void)mode; (void)flags;
    memfs_init();
    return (node_find((const char *)path_ptr) >= 0) ? 0 : -44;
}

int __syscall_mkdirat(int dirfd, intptr_t path_ptr, int mode) {
    (void)dirfd; (void)mode;
    memfs_init();
    const char *path = (const char *)path_ptr;
    if (node_find(path) >= 0) return 0;
    return (node_create(path, MEMFS_DIR) >= 0) ? 0 : -28; /* -ENOSPC */
}

int __syscall_getdents64(int fd, intptr_t dirp, int count) {
    memfs_init();
    if (fd < 3 || fd >= MEMFS_MAX_FDS || g_fds[fd].node < 0) return -9; /* -EBADF */

    MemfsFd *f = &g_fds[fd];
    MemfsNode *dir = &g_nodes[f->node];
    if (dir->type != MEMFS_DIR) return -20; /* -ENOTDIR */

    /* Build the prefix for matching direct children. */
    const char *dir_path = dir->path;
    size_t dir_len = strlen(dir_path);
    char prefix[512];
    size_t prefix_len;
    if (dir_len == 1 && dir_path[0] == '/') {
        prefix[0] = '/'; prefix[1] = '\0';
        prefix_len = 1;
    } else {
        snprintf(prefix, sizeof(prefix), "%s/", dir_path);
        prefix_len = dir_len + 1;
    }

    uint8_t *buf = (uint8_t *)dirp;
    int written = 0;
    int child_idx = 0;

    for (int i = 0; i < g_node_count; i++) {
        if (!g_nodes[i].type) continue;
        if (i == f->node) continue;

        const char *path = g_nodes[i].path;
        size_t path_len = strlen(path);
        if (path_len <= prefix_len) continue;
        if (strncmp(path, prefix, prefix_len) != 0) continue;
        /* Direct child only: no slash after the prefix. */
        if (strchr(path + prefix_len, '/') != NULL) continue;

        /* Skip already-returned entries (pos tracks child count). */
        if (child_idx < (int)f->pos) { child_idx++; continue; }

        const char *name = path + prefix_len;
        size_t name_len = strlen(name);

        /* dirent64: d_ino(8) + d_off(8) + d_reclen(2) + d_type(1) + name + NUL */
        size_t reclen = 8 + 8 + 2 + 1 + name_len + 1;
        reclen = (reclen + 7) & ~(size_t)7; /* align to 8 */
        if (written + (int)reclen > count) break;

        uint8_t *entry = buf + written;
        memset(entry, 0, reclen);

        uint64_t ino = (uint64_t)(i + 1);
        memcpy(entry, &ino, 8);                          /* d_ino */
        int64_t d_off = written + (int64_t)reclen;
        memcpy(entry + 8, &d_off, 8);                    /* d_off */
        uint16_t d_reclen = (uint16_t)reclen;
        memcpy(entry + 16, &d_reclen, 2);                /* d_reclen */
        entry[18] = (g_nodes[i].type == MEMFS_DIR) ? 4 : 8; /* d_type: DT_DIR / DT_REG */
        memcpy(entry + 19, name, name_len + 1);          /* d_name */

        written += (int)reclen;
        child_idx++;
        f->pos++;
    }

    return written;
}

int __syscall_readlinkat(int fd, intptr_t p, intptr_t b, int s) {
    (void)fd; (void)p; (void)b; (void)s;
    return -22; /* -EINVAL: not a symlink */
}

/* Remaining syscall stubs. */
int __syscall_rmdir(intptr_t p) { (void)p; return -1; }
int __syscall_unlinkat(int a, intptr_t b, int c) { (void)a; (void)b; (void)c; return -1; }
int __syscall_ftruncate64(int a, int64_t b) { (void)a; (void)b; return -1; }
int __syscall_renameat(int a, intptr_t b, int c, intptr_t d) { (void)a; (void)b; (void)c; (void)d; return -1; }
int __syscall_chdir(intptr_t p) { (void)p; return 0; }
int __syscall_chmod(intptr_t p, int m) { (void)p; (void)m; return 0; }
int __syscall_fcntl64(int fd, int cmd, ...) { (void)fd; (void)cmd; return 0; }
int __syscall_ioctl(int fd, int op, ...) { (void)fd; (void)op; return -25; /* -ENOTTY */ }

/* ── stdio_read / stdio_write overrides ─────────────────────── */
/*
 * musl's fread/fwrite call __stdio_read/__stdio_write, which in the
 * Emscripten build call __wasi_fd_read/__wasi_fd_write directly.
 * We override these so memfs fds are served from memory, while
 * stdin/stdout/stderr (fd 0/1/2) still go through WASI.
 *
 * IMPORTANT: musl's __stdio_write is called with TWO pieces of data:
 *   1. Buffered data in the FILE struct (f->wbase..f->wpos)
 *   2. New data in (buf, len)
 * The implementation must write BOTH and reset the buffer pointers.
 * Otherwise fwrite+fflush silently loses buffered data.
 *
 * musl FILE struct layout on wasm32 (from stdio_impl.h):
 *   offset  0: unsigned flags
 *   offset  4: unsigned char *rpos
 *   offset  8: unsigned char *rend
 *   offset 12: int (*close)(FILE *)
 *   offset 16: unsigned char *wend
 *   offset 20: unsigned char *wpos
 *   offset 24: unsigned char *mustbezero_1
 *   offset 28: unsigned char *wbase
 *   offset 32: size_t (*read)(...)
 *   offset 36: size_t (*write)(...)
 *   offset 40: off_t (*seek)(...)    [function pointer, 4 bytes]
 *   offset 44: unsigned char *buf
 *   offset 48: size_t buf_size
 */

/* Import the real WASI fd_read/fd_write for stdin/stdout/stderr. */
#include <wasi/api.h>

/* Access musl FILE internal fields by offset (wasm32: all pointers 4 bytes). */
#define FILE_RPOS(f)     (*(unsigned char **)((char *)(f) + 4))
#define FILE_REND(f)     (*(unsigned char **)((char *)(f) + 8))
#define FILE_WEND(f)     (*(unsigned char **)((char *)(f) + 16))
#define FILE_WPOS(f)     (*(unsigned char **)((char *)(f) + 20))
#define FILE_WBASE(f)    (*(unsigned char **)((char *)(f) + 28))
#define FILE_BUF(f)      (*(unsigned char **)((char *)(f) + 44))
#define FILE_BUF_SIZE(f) (*(size_t *)((char *)(f) + 48))

size_t __stdio_read(FILE *f, unsigned char *buf, size_t len) {
    int fd = fileno(f);
    if (fd >= 3 && fd < MEMFS_MAX_FDS && g_fds[fd].node >= 0) {
        ssize_t n = memfs_read(fd, buf, len);
        if (n <= 0) {
            FILE_FLAGS(f) |= (n < 0) ? F_ERR : F_EOF;
            return 0;
        }
        return (size_t)n;
    }
    /* WASI fd (stdin): use real WASI fd_read. */
    __wasi_iovec_t iov = { .buf = buf, .buf_len = len };
    __wasi_size_t nread = 0;
    if (__wasi_fd_read(fd, &iov, 1, &nread) != 0) {
        FILE_FLAGS(f) |= F_ERR;
        return 0;
    }
    if (nread == 0) { FILE_FLAGS(f) |= F_EOF; return 0; }
    return nread;
}

size_t __stdio_write(FILE *f, const unsigned char *buf, size_t len) {
    int fd = fileno(f);
    if (fd >= 3 && fd < MEMFS_MAX_FDS && g_fds[fd].node >= 0) {
        /* Flush musl's internal write buffer first (f->wbase..f->wpos). */
        unsigned char *wbase = FILE_WBASE(f);
        unsigned char *wpos  = FILE_WPOS(f);
        if (wpos && wbase && wpos > wbase) {
            ssize_t bw = memfs_write(fd, wbase, (size_t)(wpos - wbase));
            if (bw < 0) { FILE_FLAGS(f) |= F_ERR; return 0; }
        }
        /* Write new data. */
        if (len > 0) {
            ssize_t n = memfs_write(fd, buf, len);
            if (n < 0) { FILE_FLAGS(f) |= F_ERR; return 0; }
        }
        /* Reset buffer pointers so musl knows the buffer is empty. */
        unsigned char *fbuf = FILE_BUF(f);
        size_t buf_size     = FILE_BUF_SIZE(f);
        FILE_WPOS(f)  = fbuf;
        FILE_WBASE(f) = fbuf;
        FILE_WEND(f)  = fbuf + buf_size;
        return len;
    }
    /* WASI fd (stdout/stderr): write both buffer and new data. */
    unsigned char *wbase = FILE_WBASE(f);
    unsigned char *wpos  = FILE_WPOS(f);
    __wasi_ciovec_t iovs[2];
    int iovcnt = 0;
    if (wpos && wbase && wpos > wbase) {
        iovs[iovcnt].buf = wbase;
        iovs[iovcnt].buf_len = (size_t)(wpos - wbase);
        iovcnt++;
    }
    if (len > 0) {
        iovs[iovcnt].buf = buf;
        iovs[iovcnt].buf_len = len;
        iovcnt++;
    }
    __wasi_size_t nwritten = 0;
    if (iovcnt > 0 && __wasi_fd_write(fd, iovs, iovcnt, &nwritten) != 0) {
        FILE_FLAGS(f) |= F_ERR;
        return 0;
    }
    /* Reset buffer pointers. */
    unsigned char *fbuf = FILE_BUF(f);
    size_t buf_size     = FILE_BUF_SIZE(f);
    FILE_WPOS(f)  = fbuf;
    FILE_WBASE(f) = fbuf;
    FILE_WEND(f)  = fbuf + buf_size;
    return len;
}

size_t __stdio_seek(FILE *f, off_t off, int whence) {
    int fd = fileno(f);
    if (fd >= 3 && fd < MEMFS_MAX_FDS && g_fds[fd].node >= 0) {
        MemfsNode *n = &g_nodes[g_fds[fd].node];
        uint32_t new_pos;
        switch (whence) {
            case 0: new_pos = (uint32_t)off; break;
            case 1: new_pos = g_fds[fd].pos + (int32_t)off; break;
            case 2: new_pos = n->size + (int32_t)off; break;
            default: return -1;
        }
        g_fds[fd].pos = new_pos;
        return new_pos;
    }
    return -1;
}

int __stdio_close(FILE *f) {
    int fd = fileno(f);
    if (fd >= 3 && fd < MEMFS_MAX_FDS) {
        g_fds[fd].node = -1;
        return 0;
    }
    return 0;
}

/* Also override read/write/close/lseek for direct syscall-level use. */
ssize_t read(int fd, void *buf, size_t count) {
    if (fd >= 3 && fd < MEMFS_MAX_FDS && g_fds[fd].node >= 0)
        return memfs_read(fd, buf, count);
    __wasi_iovec_t iov = { .buf = buf, .buf_len = count };
    __wasi_size_t nr = 0;
    if (__wasi_fd_read(fd, &iov, 1, &nr) != 0) return -1;
    return nr;
}

ssize_t write(int fd, const void *buf, size_t count) {
    if (fd >= 3 && fd < MEMFS_MAX_FDS && g_fds[fd].node >= 0)
        return memfs_write(fd, buf, count);
    __wasi_ciovec_t iov = { .buf = buf, .buf_len = count };
    __wasi_size_t nw = 0;
    if (__wasi_fd_write(fd, &iov, 1, &nw) != 0) return -1;
    return nw;
}

int close(int fd) {
    if (fd >= 3 && fd < MEMFS_MAX_FDS && g_fds[fd].node >= 0) {
        g_fds[fd].node = -1;
        return 0;
    }
    return 0;
}

off_t lseek(int fd, off_t offset, int whence) {
    if (fd >= 3 && fd < MEMFS_MAX_FDS && g_fds[fd].node >= 0) {
        MemfsNode *n = &g_nodes[g_fds[fd].node];
        uint32_t p;
        switch (whence) {
            case 0: p = offset; break;
            case 1: p = g_fds[fd].pos + offset; break;
            case 2: p = n->size + offset; break;
            default: return -1;
        }
        g_fds[fd].pos = p;
        return p;
    }
    return -1;
}
