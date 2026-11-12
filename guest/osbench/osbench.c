// SPDX-License-Identifier: GPL-3.0-or-later
// OS-heavy guest workload for host-performance measurement: user-mode CPU
// work, exec allocator calls, small-file filesystem traffic and process
// launches, the mix a build tool produces. It needs no NDK headers: library
// calls go through the two register stubs in start.s.
//
//   osbench <dir> <phases> <scale>
//
// <dir> is a directory or volume to work in ("DH0:", "RunProg:tmp/"), with
// its trailing ':' or '/'. <phases> is any of: F format <dir> as an FFS
// volume first, c CPU, s exec calls, f filesystem, p process launches.
// <scale> multiplies every phase's iteration count. Each phase prints the
// emulated time it took in 1/50 s ticks; the same lines are appended to
// osbench.log in <dir>'s parent program directory when it can be opened.
// "osbench x <file>" is the child the launch phase runs: it creates and
// deletes <file> and returns.

typedef long LONG;
typedef unsigned long ULONG;
typedef unsigned char UBYTE;

extern LONG dos4(LONG lvo, LONG d1, LONG d2, LONG d3, LONG d4);
extern LONG exec4(LONG lvo, LONG d0, LONG d1, LONG a0, LONG a1);

void *DOSBase;

#define OpenLibrary(n, v) ((void *)exec4(-552, (v), 0, 0, (LONG)(n)))
#define CloseLibrary(b) exec4(-414, 0, 0, 0, (LONG)(b))
#define AllocMem(s, f) ((void *)exec4(-198, (LONG)(s), (LONG)(f), 0, 0))
#define FreeMem(p, s) exec4(-210, (LONG)(s), 0, 0, (LONG)(p))

#define Open(n, m) dos4(-30, (LONG)(n), (m), 0, 0)
#define Close(f) dos4(-36, (f), 0, 0, 0)
#define Read(f, b, l) dos4(-42, (f), (LONG)(b), (l), 0)
#define Write(f, b, l) dos4(-48, (f), (LONG)(b), (l), 0)
#define Output() dos4(-60, 0, 0, 0, 0)
#define DeleteFile(n) dos4(-72, (LONG)(n), 0, 0, 0)
#define Rename(a, b) dos4(-78, (LONG)(a), (LONG)(b), 0, 0)
#define Lock(n, m) dos4(-84, (LONG)(n), (m), 0, 0)
#define UnLock(l) dos4(-90, (l), 0, 0, 0)
#define Examine(l, f) dos4(-102, (l), (LONG)(f), 0, 0)
#define ExNext(l, f) dos4(-108, (l), (LONG)(f), 0, 0)
#define CreateDir(n) dos4(-120, (LONG)(n), 0, 0, 0)
#define IoErr() dos4(-132, 0, 0, 0, 0)
#define DateStamp(d) dos4(-192, (LONG)(d), 0, 0, 0)
#define Delay(t) dos4(-198, (t), 0, 0, 0)
#define SystemTagList(c, t) dos4(-606, (LONG)(c), (LONG)(t), 0, 0)
#define Format(f, v, t) dos4(-714, (LONG)(f), (LONG)(v), (LONG)(t), 0)
#define Inhibit(n, o) dos4(-726, (LONG)(n), (o), 0, 0)

#define MODE_OLDFILE 1005
#define MODE_NEWFILE 1006
#define ACCESS_READ (-2)
#define MEMF_ANY 0
#define MEMF_PUBLIC 1
#define ID_FFS_DISK 0x444F5301UL

void *memset(void *d, int c, ULONG n)
{
    UBYTE *p = d;
    while (n--)
        *p++ = (UBYTE)c;
    return d;
}

void *memcpy(void *d, const void *s, ULONG n)
{
    UBYTE *p = d;
    const UBYTE *q = s;
    while (n--)
        *p++ = *q++;
    return d;
}

static ULONG slen(const char *s)
{
    ULONG n = 0;
    while (s[n])
        n++;
    return n;
}

static LONG out, logfile;

static void put(const char *s)
{
    ULONG n = slen(s);
    if (out)
        Write(out, s, n);
    if (logfile)
        Write(logfile, s, n);
}

static void putnum(ULONG v)
{
    char buf[12];
    int i = 11;
    buf[i] = 0;
    do {
        buf[--i] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    put(buf + i);
}

static char *cat(char *d, const char *s)
{
    while (*s)
        *d++ = *s++;
    *d = 0;
    return d;
}

static char *catnum(char *d, ULONG v)
{
    char buf[12];
    int i = 11;
    buf[i] = 0;
    do {
        buf[--i] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    return cat(d, buf + i);
}

static ULONG ticks(void)
{
    ULONG ds[3];
    DateStamp(ds);
    return (ds[0] * 1440 + ds[1]) * 3000 + ds[2];
}

static void report(const char *name, ULONG start, ULONG check)
{
    put(name);
    put(" ticks ");
    putnum(ticks() - start);
    put(" check ");
    putnum(check);
    put("\n");
}

/* ---- CPU phase: the shape of a compiler pass ------------------------- */

#define TEXT_BYTES 32768
#define HASH_SLOTS 4096

struct sym {
    struct sym *next;
    ULONG hash;
    ULONG count;
    UBYTE len;
    char name[11];
};

static ULONG crc_table[256];
static UBYTE *text;
static struct sym **slots;
static struct sym *pool;
static ULONG pool_used;
static ULONG seed = 12345;

static ULONG rnd(void)
{
    seed = seed * 1103515245UL + 12345UL;
    return seed >> 8;
}

static ULONG crc32(const UBYTE *p, ULONG n)
{
    ULONG c = 0xFFFFFFFFUL;
    while (n--)
        c = crc_table[(c ^ *p++) & 0xFF] ^ (c >> 8);
    return ~c;
}

static struct sym *intern(const UBYTE *s, ULONG len)
{
    ULONG h = 5381, i;
    struct sym *e;
    for (i = 0; i < len; i++)
        h = h * 33 + s[i];
    for (e = slots[h & (HASH_SLOTS - 1)]; e; e = e->next) {
        if (e->hash == h && e->len == len) {
            for (i = 0; i < len && (UBYTE)e->name[i] == s[i]; i++)
                ;
            if (i == len)
                return e;
        }
    }
    e = &pool[pool_used++];
    e->hash = h;
    e->len = (UBYTE)len;
    e->count = 0;
    for (i = 0; i < len; i++)
        e->name[i] = (char)s[i];
    e->next = slots[h & (HASH_SLOTS - 1)];
    slots[h & (HASH_SLOTS - 1)] = e;
    return e;
}

static ULONG __attribute__((noinline)) walk(ULONG depth, ULONG v)
{
    if (depth == 0)
        return v ^ (v >> 3);
    return walk(depth - 1, v * 3 + 1) + walk(depth - 1, v + depth);
}

static ULONG cpu_phase(ULONG rounds)
{
    ULONG check = 0, r, i;
    for (i = 0; i < 256; i++) {
        ULONG c = i, k;
        for (k = 0; k < 8; k++)
            c = (c & 1) ? 0xEDB88320UL ^ (c >> 1) : c >> 1;
        crc_table[i] = c;
    }
    text = AllocMem(TEXT_BYTES, MEMF_ANY);
    slots = AllocMem(HASH_SLOTS * sizeof(*slots), MEMF_ANY);
    pool = AllocMem(8192 * sizeof(*pool), MEMF_ANY);
    if (!text || !slots || !pool)
        return 0;
    for (r = 0; r < rounds; r++) {
        ULONG pos = 0;
        /* "source text": identifiers from a small vocabulary */
        for (i = 0; i < TEXT_BYTES; i++) {
            ULONG v = rnd();
            text[i] = (v & 7) == 0 ? ' ' : (UBYTE)('a' + (v >> 3) % 6);
        }
        memset(slots, 0, HASH_SLOTS * sizeof(*slots));
        pool_used = 0;
        while (pos < TEXT_BYTES && pool_used < 8000) {
            ULONG start;
            while (pos < TEXT_BYTES && text[pos] == ' ')
                pos++;
            start = pos;
            while (pos < TEXT_BYTES && text[pos] != ' ' && pos - start < 10)
                pos++;
            if (pos > start)
                intern(text + start, pos - start)->count++;
        }
        check += crc32(text, TEXT_BYTES) + pool_used + walk(12, r);
    }
    FreeMem(pool, 8192 * sizeof(*pool));
    FreeMem(slots, HASH_SLOTS * sizeof(*slots));
    FreeMem(text, TEXT_BYTES);
    return check;
}

/* ---- exec phase: allocator traffic in Kickstart code ----------------- */

static ULONG sys_phase(ULONG rounds)
{
    void *held[16];
    ULONG check = 0, r, i;
    for (r = 0; r < rounds; r++) {
        for (i = 0; i < 16; i++) {
            ULONG size = 24 + ((r * 7 + i * 131) & 0x3F8);
            held[i] = AllocMem(size, MEMF_PUBLIC);
            if (held[i]) {
                *(ULONG *)held[i] = size;
                check += size;
            }
        }
        for (i = 0; i < 16; i++) {
            ULONG k = (i * 5) & 15;
            if (held[k])
                FreeMem(held[k], *(ULONG *)held[k]);
        }
    }
    return check;
}

/* ---- filesystem phase: many small files ------------------------------ */

static char path_a[160], path_b[160];
static ULONG fib[70]; /* struct FileInfoBlock is 260 bytes, long aligned */
static UBYTE filebuf[4096];

static ULONG fs_phase(const char *dir, ULONG rounds, ULONG files)
{
    ULONG check = 0, r, i;
    char *end;
    LONG fh, lock;
    for (i = 0; i < sizeof(filebuf); i++)
        filebuf[i] = (UBYTE)(i * 7);
    for (r = 0; r < rounds; r++) {
        end = cat(path_a, dir);
        cat(end, "obj");
        lock = CreateDir(path_a);
        if (lock)
            UnLock(lock);
        for (i = 0; i < files; i++) {
            end = cat(path_a, dir);
            end = cat(end, "obj/file");
            end = catnum(end, i);
            cat(end, ".o");
            fh = Open(path_a, MODE_NEWFILE);
            if (fh) {
                check += (ULONG)Write(fh, filebuf, 300 + (i * 37) % 3700);
                Close(fh);
            }
        }
        end = cat(path_a, dir);
        cat(end, "obj");
        lock = Lock(path_a, ACCESS_READ);
        if (lock) {
            if (Examine(lock, fib))
                while (ExNext(lock, fib))
                    check += fib[31]; /* fib_Size */
            UnLock(lock);
        }
        for (i = 0; i < files; i++) {
            end = cat(path_a, dir);
            end = cat(end, "obj/file");
            end = catnum(end, i);
            cat(end, ".o");
            fh = Open(path_a, MODE_OLDFILE);
            if (fh) {
                LONG n;
                while ((n = Read(fh, filebuf, sizeof(filebuf))) > 0)
                    check += (ULONG)n + filebuf[n - 1];
                Close(fh);
            }
            end = cat(path_b, dir);
            end = cat(end, "obj/dep");
            end = catnum(end, i);
            cat(end, ".d");
            check += (ULONG)(Rename(path_a, path_b) != 0);
        }
        for (i = 0; i < files; i++) {
            end = cat(path_b, dir);
            end = cat(end, "obj/dep");
            end = catnum(end, i);
            cat(end, ".d");
            lock = Lock(path_b, ACCESS_READ);
            if (lock)
                UnLock(lock);
            check += (ULONG)(DeleteFile(path_b) != 0);
        }
        end = cat(path_a, dir);
        cat(end, "obj");
        check += (ULONG)(DeleteFile(path_a) != 0);
    }
    return check;
}

/* ---- launch phase: one short-lived command per file ------------------ */

static ULONG copy_self(const char *from, const char *to)
{
    LONG in = Open(from, MODE_OLDFILE), o, n;
    ULONG total = 0;
    if (!in)
        return 0;
    o = Open(to, MODE_NEWFILE);
    if (o) {
        while ((n = Read(in, filebuf, sizeof(filebuf))) > 0)
            total += (ULONG)Write(o, filebuf, n);
        Close(o);
    }
    Close(in);
    return total;
}

static ULONG spawn_phase(const char *dir, ULONG rounds)
{
    static char cmd[200];
    ULONG check = 0, r;
    char *end;
    end = cat(path_a, dir);
    cat(end, "osbench");
    if (!copy_self("RunProg:osbench", path_a))
        return 0;
    for (r = 0; r < rounds; r++) {
        end = cat(cmd, path_a);
        end = cat(end, " x ");
        end = cat(end, dir);
        end = cat(end, "stamp");
        catnum(end, r & 7);
        check += (ULONG)(SystemTagList(cmd, 0) == 0);
    }
    DeleteFile(path_a);
    return check;
}

static ULONG number(const char **p)
{
    ULONG v = 0;
    while (**p == ' ')
        (*p)++;
    while (**p >= '0' && **p <= '9')
        v = v * 10 + (ULONG)(*(*p)++ - '0');
    return v;
}

static void word(const char **p, char *dst, ULONG max)
{
    ULONG n = 0;
    while (**p == ' ')
        (*p)++;
    while (**p && **p != ' ' && **p != '\n' && n + 1 < max)
        dst[n++] = *(*p)++;
    dst[n] = 0;
}

LONG entry(const char *args, LONG len)
{
    static char line[200], dir[100], phases[16];
    const char *p = line;
    ULONG scale, start, total, i;
    LONG rc = 0;

    DOSBase = OpenLibrary("dos.library", 36);
    if (!DOSBase)
        return 20;
    for (i = 0; i < (ULONG)len && i + 1 < sizeof(line); i++)
        line[i] = args[i];
    line[i] = 0;
    word(&p, dir, sizeof(dir));
    if (dir[0] == 'x' && dir[1] == 0) {
        LONG fh;
        word(&p, dir, sizeof(dir));
        fh = Open(dir, MODE_NEWFILE);
        if (fh) {
            Write(fh, "x", 1);
            Close(fh);
        }
        DeleteFile(dir);
        CloseLibrary(DOSBase);
        return 0;
    }
    word(&p, phases, sizeof(phases));
    scale = number(&p);
    if (scale == 0)
        scale = 1;
    out = Output();
    logfile = Open("RunProg:osbench.log", MODE_NEWFILE);
    total = ticks();
    for (i = 0; phases[i]; i++) {
        start = ticks();
        switch (phases[i]) {
        case 'F': {
            LONG ok;
            Inhibit(dir, -1);
            ok = Format(dir, "Bench", ID_FFS_DISK);
            Inhibit(dir, 0);
            Delay(50);
            report("format", start, (ULONG)ok);
            if (!ok)
                rc = 10;
            break;
        }
        case 'c':
            report("cpu", start, cpu_phase(40 * scale));
            break;
        case 's':
            report("exec", start, sys_phase(4000 * scale));
            break;
        case 'f':
            report("files", start, fs_phase(dir, 2 * scale, 100));
            break;
        case 'p':
            report("launch", start, spawn_phase(dir, 40 * scale));
            break;
        }
    }
    report("total", total, scale);
    if (logfile)
        Close(logfile);
    CloseLibrary(DOSBase);
    return rc;
}
