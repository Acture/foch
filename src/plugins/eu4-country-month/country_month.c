#define _WIN32_WINNT 0x0601
#include <windows.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <assert.h>
#include <limits.h>
#include "foch_plugin.h"

static int daily_a_readable(void *image, size_t image_size, const void *address, size_t bytes);
static int daily_a_target(const unsigned char *insn, size_t len, int32_t rel, void **target);

static int memory_readable(const void *address, size_t bytes) {
	uintptr_t at = (uintptr_t)address, end;
	if (!address || bytes > UINTPTR_MAX - at) return 0;
	end = at + bytes;
	while (at < end) {
		MEMORY_BASIC_INFORMATION region;
		uintptr_t next;
		if (!VirtualQuery((void *)at, &region, sizeof(region)) || region.State != MEM_COMMIT ||
			(region.Protect & (PAGE_GUARD | PAGE_NOACCESS)) ||
			!(region.Protect & (PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY |
				PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)) ||
			region.RegionSize > UINTPTR_MAX - (uintptr_t)region.BaseAddress) return 0;
		next = (uintptr_t)region.BaseAddress + region.RegionSize;
		if (next <= at) return 0;
		at = next < end ? next : end;
	}
	return 1;
}

static int image_code(const FochHostApi *host, const void *address) {
	MEMORY_BASIC_INFORMATION region;
	return daily_a_readable(host->exe_base, host->exe_size, address, 1) &&
		VirtualQuery(address, &region, sizeof(region)) &&
		(region.Protect & (PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY));
}

/* Complete native spans bypassed by the detours. Only the monthly
 * function's rel32 operand varies; it is resolved and checked before enable.
 * The tail's second call, branches, and all header instructions stay exact. */
static const unsigned char monthly_image[] = {
	0x49,0x8B,0x87,0xE8,0x1C,0x00,0x00,0x45,0x0F,0xB6,0xC4,0x41,0x0F,0xB6,0xD5,0x48,
	0x8B,0x0C,0xD8,0xE8,0x78,0x59,0xB9,0xFF,0xB8,0x67,0x66,0x66,0x66,0xF7,0xEF,0xD1,
	0xFA,0x8B,0xC2,0xC1,0xE8,0x1F,0x03,0xD0,0x8D,0x0C,0x92,0x3B,0xF9,0x75,0x10,0x33,
	0xD2,0x41,0xB9,0xF8,0x10,0x00,0x00,0x49,0x8B,0xCF,0xE8,0x31,0xC1,0x00,0x00,0xFF,
	0xC7,0x48,0xFF,0xC3,0x48,0x3B,0xDE,0x7C,0xB7,
};
static const unsigned char active_header_image[] = {
	0x44,0x8B,0xE7,0x48,0x8B,0x8D,0xE8,0x1C,0x00,0x00,0x48,0x8B,0x85,0xF0,0x1C,0x00,
	0x00,0x48,0x2B,0xC1,0x48,0xC1,0xF8,0x03,0x85,0xC0,0x0F,0x8E,0xCE,0x01,0x00,0x00,
	0x0F,0x1F,0x44,0x00,0x00,
};

static int monthly_bytes_match(const unsigned char *code, const unsigned char *profile, size_t bytes) {
	return bytes == 0x49 && memcmp(code, profile, 0x14) == 0 &&
		memcmp(code + 0x18, profile + 0x18, bytes - 0x18) == 0;
}

static int monthly_profile_is_known(const FochHostApi *host, const unsigned char *code) {
	if (!daily_a_readable(host->exe_base, host->exe_size, code, sizeof(monthly_image))) return 0;
	if (monthly_bytes_match(code, monthly_image, sizeof(monthly_image))) {
		int32_t rel;
		void *target;
		memcpy(&rel, code + 0x3B, sizeof(rel));
		return daily_a_target(code + 0x3A, 5, rel, &target) && image_code(host, target);
	}
#ifdef FOCH_MONTHLY_FIXTURE_PROFILE
	return monthly_bytes_match(code, FOCH_MONTHLY_FIXTURE_PROFILE, sizeof(FOCH_MONTHLY_FIXTURE_PROFILE));
#else
	return 0;
#endif
}

static int active_header_is_known(const unsigned char *code, int fixture) {
	if (!memory_readable(code, sizeof(active_header_image))) return 0;
	if (!fixture) return memcmp(code, active_header_image, sizeof(active_header_image)) == 0;
#ifdef FOCH_ACTIVE_FIXTURE_HEADER
	return sizeof(FOCH_ACTIVE_FIXTURE_HEADER) == sizeof(active_header_image) &&
		memcmp(code, FOCH_ACTIVE_FIXTURE_HEADER, sizeof(active_header_image)) == 0;
#else
	return 0;
#endif
}

/* The country-month loop head is the scheduling hook. rbx is the first index,
   rsi the end, r12b and r13b the two constant arguments. Workers call the
   state function through its trampoline, one country at a time. The game
   thread then replays the complete known monthly loop in its original
   frame, including the conditional helper and live EDI updates. After
   precomputation its state calls return immediately; native fallback
   and the serial control perform those state calls in the original loop.

   The daily active-country loop is a separate hook. On the installed game the
   original loop body is relocated and run on the game thread, so its eight
   direct calls stay the game's functions. The fixture plants a magic dword in
   front of its stand-in predicate; that path still records one decision per
   country on the pool and appends on the game thread. */

#define MAX_WORKERS 32
#define COUNTRY_ARRAY 0x1CE8
#define COUNTRY_END 0x1CF0
#define STATE_BEGIN 0x24B0
#define STATE_END 0x24B8
#define ACTIVE_TAG 0x20
#define ACTIVE_D28 0xD28
#define ACTIVE_KIND 0x1344
#define ACTIVE_AUX_SIZE 0x1D14
#define ACTIVE_LIST1 0x1D18
#define ACTIVE_LIST2 0x1D30
#define ACTIVE_SUM_STEP 0x3E8u

typedef void (*MonthlyFn)(void *country, unsigned dl, unsigned r8_arg);
typedef void (*StateFn)(void *state);

struct work_part {
	void **countries;
	uint64_t from;
	uint64_t to;
	DWORD tid;
};

extern void country_detour(void);
extern void state_detour(void);
extern void active_detour(void);
extern void active_body_return(void);
extern void active_invoke_back(void);
extern uint32_t active_invoke(void *entry, void *game, unsigned char flag);
extern void *country_site;
extern void *country_after;
extern void *country_body;
extern void country_replay_done(void);
extern void *active_site;
extern void *active_after;
extern void *active_body;
extern void *const *country_next;
extern void *const *state_next;
extern void *const *active_next;
extern volatile LONG country_splitting;
extern volatile LONG states_done;
extern volatile LONG active_running;
extern volatile LONG active_enter_body;

typedef int (*PredFn)(void *country);

struct active_decision {
	uint64_t tag;
	DWORD tid;
	int list1;
	int gate2;
};

struct active_part {
	void **countries;
	struct active_decision *rows;
	uint32_t start;
	uint32_t flag;
	uint64_t from;
	uint64_t to;
};

static MonthlyFn country_monthly;
static FochHook *country_hook;
static FochHook *state_hook;
static FochHook *active_hook;
/* Hooks start disabled in the SDK. Prepare every member before publishing
 * any callback; on an enable error also disable the failed attempt, since a
 * host may report failure after updating its chain. The host owns all cells. */
struct owned_hook { FochHook *handle; int queued; };
static struct owned_hook owned_hooks[12];
static unsigned owned_count, owned_order[12], owned_pending;
static int owned_preparing;
static FochStatus plugin_status = { sizeof(FochStatus), FOCH_STATE_INITIALIZING, 0, { NULL, 0 } };

static int32_t owned_create(const FochHostApi *host, void *target, void *detour,
	int32_t priority, void *const **next, FochHook **hook) {
	int32_t rc;
	if (owned_preparing && owned_count == sizeof(owned_hooks) / sizeof(owned_hooks[0])) return FOCH_E_MEMORY;
	*hook = NULL;
	rc = host->hook_create(host, target, detour, priority, next, hook);
	if (rc != FOCH_OK) return rc;
	if (!*hook) return FOCH_E_INVALID;
	if (owned_preparing) owned_hooks[owned_count++] = (struct owned_hook){ *hook, 0 };
	return FOCH_OK;
}

static int32_t owned_enable(const FochHostApi *host, FochHook *hook) {
	unsigned i;
	if (!owned_preparing) return host->hook_set_enabled(host, hook, 1);
	for (i = 0; i < owned_count; ++i) if (owned_hooks[i].handle == hook) {
		if (!owned_hooks[i].queued) {
			owned_order[owned_pending++] = i;
			owned_hooks[i].queued = 1;
		}
		return FOCH_OK;
	}
	return FOCH_E_INVALID;
}

static int32_t owned_commit(const FochHostApi *host) {
	unsigned i;
	for (i = 0; i < owned_pending; ++i) {
		int32_t rc = host->hook_set_enabled(host, owned_hooks[owned_order[i]].handle, 1);
		if (rc != FOCH_OK) return rc;
	}
	return FOCH_OK;
}

static int32_t owned_rollback(const FochHostApi *host) {
	unsigned i = owned_pending;
	int32_t result = FOCH_OK;
	while (i) {
		int32_t rc = host->hook_set_enabled(host, owned_hooks[owned_order[--i]].handle, 0);
		if (rc != FOCH_OK && result == FOCH_OK) result = rc;
	}
	i = owned_count;
	while (i) {
		struct owned_hook *hook = &owned_hooks[--i];
		if (!hook->queued) {
			int32_t rc = host->hook_set_enabled(host, hook->handle, 0);
			if (rc != FOCH_OK && result == FOCH_OK) result = rc;
		}
	}
	return result;
}

static int state_async;
static PredFn active_pred;
static int active_real;
static int32_t active_g0;
static int32_t active_g1;
static int32_t active_g2;
static PTP_POOL active_pool;
static PTP_CLEANUP_GROUP active_group;
static TP_CALLBACK_ENVIRON active_environment;
static int active_pool_ready;

/* The game may change its working directory after startup, so proofs are
   written through the directory the plugin DLL was staged in: the runtime
   layer stages <layer>\plugins\<package>\eu4_country_month.dll, the test
   runtime stages <rt>\plugins\eu4_country_month.dll. Either way the layer
   is the parent of the "plugins" directory, captured once at entry init. */
static char proof_root[MAX_PATH];

static void capture_proof_root(void) {
	HMODULE self = NULL;
	wchar_t wide[MAX_PATH];
	char path[MAX_PATH];
	char *dir;
	char *plugins;
	if (proof_root[0]) return;
	if (!GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
			(LPCWSTR)(void *)&capture_proof_root, &self) || !self) return;
	if (!GetModuleFileNameW(self, wide, MAX_PATH)) return;
	if (WideCharToMultiByte(CP_UTF8, 0, wide, -1, path, sizeof(path), NULL, NULL) <= 0) return;
	dir = path + strlen(path);
	while (dir > path && dir[-1] != '\\' && dir[-1] != '/') dir -= 1; /* file name */
	if (dir == path) return;
	dir -= 1; /* the separator before it */
	plugins = dir;
	while (plugins > path && plugins[-1] != '\\' && plugins[-1] != '/') plugins -= 1;
	if (strcmp(plugins, "plugins") != 0) return; /* dir already ...\<rt>\plugins */
	dir = plugins;
	while (dir > path && dir[-1] != '\\' && dir[-1] != '/') dir -= 1; /* the plugins component */
	if (dir == path) return;
	memcpy(proof_root, path, (size_t)(dir - path));
	proof_root[dir - path] = 0;
}

static void write_proof(const char *name, const char *text) {
	char path[MAX_PATH + 64];
	HANDLE file;
	DWORD wrote = 0;
	if (proof_root[0]) snprintf(path, sizeof(path), "%s\\foch-host\\%s", proof_root, name);
	else snprintf(path, sizeof(path), "foch-host\\%s", name);
	file = CreateFileA(path, GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (file == INVALID_HANDLE_VALUE) return;
	WriteFile(file, text, (DWORD)strlen(text), &wrote, NULL);
	CloseHandle(file);
}

/* Real-game run proofs: the monthly split reports its span every entry and
   the daily relocation counts the days it armed. The fixture path has its
   own proofs; these two only fire against the installed game. */
static volatile LONG active_real_days;
static LONG country_split_entries;
static int serial_control = -1; /* FOCH_COUNTRY_SERIAL seen: 1 forced, 0 not */

static long long qpc_ticks_now(void) {
	LARGE_INTEGER now;
	QueryPerformanceCounter(&now);
	return now.QuadPart;
}

static long long qpc_ticks_to_ns(long long ticks) {
	LARGE_INTEGER freq;
	QueryPerformanceFrequency(&freq);
	if (freq.QuadPart <= 0) return 0;
	return (ticks / freq.QuadPart) * 1000000000LL
		+ (ticks % freq.QuadPart) * 1000000000LL / freq.QuadPart;
}

static void call_state(void *object) {
	void *const *cell = state_next;
	StateFn fn;
	if (!object || !cell) return;
	fn = (StateFn)(*cell);
	if (fn) fn(object);
}

static void country_states(void *country) {
	unsigned char *begin;
	unsigned char *end;
	if (!country) return;
	memcpy(&begin, (unsigned char *)country + STATE_BEGIN, sizeof(begin));
	memcpy(&end, (unsigned char *)country + STATE_END, sizeof(end));
	if (!begin || !end || end < begin) return;
	while (begin + sizeof(void *) <= end) {
		void *object;
		memcpy(&object, begin, sizeof(object));
		call_state(object);
		begin += sizeof(void *);
	}
}

/* State workers join before the original game-thread monthly loop. Only
 * that loop owns the primary/cadence callbacks and their live ABI context. */
static VOID CALLBACK run_part(PTP_CALLBACK_INSTANCE instance, PVOID param, PTP_WORK work) {
	struct work_part *part = param;
	uint64_t i;
	(void)instance;
	(void)work;
	part->tid = GetCurrentThreadId();
	for (i = part->from; i < part->to; ++i) {
		country_states(part->countries[i]);
	}
}

static int state_ready(void) {
	void *const *cell = state_next;
	return state_async && cell && *cell;
}

#ifndef COUNTRY_PARTS_ALLOC
#define COUNTRY_PARTS_ALLOC(n) HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, (n))
#endif
#ifndef COUNTRY_POOL_CREATE
#define COUNTRY_POOL_CREATE() CreateThreadpool(NULL)
#endif
#ifndef COUNTRY_WORK_CREATE
#define COUNTRY_WORK_CREATE(fn,p,e) CreateThreadpoolWork((fn),(p),(e))
#endif

void country_split(void *game, uint64_t index, uint64_t end) {
	struct work_part *parts = NULL;
	PTP_WORK *works = NULL;
	PTP_POOL pool = NULL;
	PTP_CLEANUP_GROUP group = NULL;
	TP_CALLBACK_ENVIRON environment;
	SYSTEM_INFO info;
	uint64_t cursor;
	uint64_t count;
	long long t0;
	int n;
	int i;
	int submitted = 0;
	int environment_ready = 0;
	int use_states;
	int prepared = 0;
	void **countries;
	if (end <= index || !country_monthly || !game) return;
	memcpy(&countries, (unsigned char *)game + COUNTRY_ARRAY, sizeof(countries));
	if (!countries) return;
	count = end - index;
	use_states = state_ready();
	InterlockedExchange(&states_done, 0);
	GetSystemInfo(&info);
	n = (int)info.dwNumberOfProcessors;
	if (n < 2) n = 2;
	if ((uint64_t)n > count) n = (int)count;
	if (n > MAX_WORKERS) n = MAX_WORKERS;
	if (n < 1) return;
	/* FOCH_COUNTRY_SERIAL=1 runs the same range serially on the game thread
	   instead of the pool, an A/B control for the span the proof reports. */
	if (serial_control < 0) {
		char value[8];
		DWORD len = GetEnvironmentVariableA("FOCH_COUNTRY_SERIAL", value, sizeof(value));
		serial_control = (len > 0 && len < sizeof(value) && value[0] == '1') ? 1 : 0;
	}
	t0 = qpc_ticks_now();
	/* The serial control and pre-work failures use the complete native
	 * loop, including its original state/monthly/helper interleaving. */
	if (serial_control || !use_states) goto done;
	parts = COUNTRY_PARTS_ALLOC(sizeof(*parts) * (size_t)n);
	works = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*works) * (size_t)n);
	pool = COUNTRY_POOL_CREATE();
	group = pool ? CreateThreadpoolCleanupGroup() : NULL;
	if (!parts || !works || !pool || !group) goto done;
	InitializeThreadpoolEnvironment(&environment);
	environment_ready = 1;
	SetThreadpoolThreadMaximum(pool, (DWORD)n);
	if (!SetThreadpoolThreadMinimum(pool, (DWORD)n) && !SetThreadpoolThreadMinimum(pool, 1)) goto done;
	SetThreadpoolCallbackPool(&environment, pool);
	SetThreadpoolCallbackCleanupGroup(&environment, group, NULL);
	cursor = index;
	for (i = 0; i < n; ++i) {
		uint64_t share = (count - (cursor - index)) / (uint64_t)(n - i);
		parts[i].countries = countries;
		parts[i].from = cursor;
		parts[i].to = cursor + share;
		parts[i].tid = 0;
		cursor += share;
		works[i] = COUNTRY_WORK_CREATE(run_part, &parts[i], &environment);
		if (!works[i]) run_part(NULL, &parts[i], NULL);
		else {
			SubmitThreadpoolWork(works[i]);
			submitted += 1;
		}
	}
	if (submitted) CloseThreadpoolCleanupGroupMembers(group, FALSE, NULL);
	prepared = 1;
done: {
		char text[192];
		LONG entries = InterlockedIncrement(&country_split_entries);
		int serial = serial_control ? 2 : (pool && group ? 0 : 1);
		snprintf(text, sizeof(text),
			"entries=%ld\ncountries=%llu\nworkers=%d\nsubmitted=%d\nstates=%d\nserial=%d\nspan-ns=%lld\n",
			entries, (unsigned long long)count, n, submitted, use_states,
			serial, qpc_ticks_to_ns(qpc_ticks_now() - t0));
		write_proof("countries-real.txt", text);
	}
	if (group) CloseThreadpoolCleanupGroup(group);
	if (environment_ready) DestroyThreadpoolEnvironment(&environment);
	if (pool) CloseThreadpool(pool);
	if (parts) HeapFree(GetProcessHeap(), 0, parts);
	if (works) HeapFree(GetProcessHeap(), 0, works);
	InterlockedExchange(&states_done, prepared);
}

static int list1_keep(uint64_t tag, uint32_t field_1344, uint32_t field_d28) {
	unsigned high = (unsigned)(tag >> 48);
	int mid = (int16_t)(tag >> 32);
	if (high == 7) return 0;
	if (field_1344 != 0) return 1;
	if (mid == active_g0 || mid == active_g1 || mid == active_g2) return 1;
	return field_d28 != 0;
}

static int list2_gate(uint32_t flag, uint64_t tag, uint32_t field_1344) {
	unsigned high = (unsigned)(tag >> 48);
	if (flag == 0) return 0;
	return field_1344 != 0 || high == 7;
}

static void judge_country(void *country, uint32_t flag, struct active_decision *row) {
	uint64_t tag = 0;
	uint32_t field_1344 = 0;
	uint32_t field_d28 = 0;
	unsigned char *bytes = country;
	row->tid = GetCurrentThreadId();
	if (!country) {
		row->tag = 0;
		row->list1 = 0;
		row->gate2 = 0;
		return;
	}
	memcpy(&tag, bytes + ACTIVE_TAG, sizeof(tag));
	memcpy(&field_d28, bytes + ACTIVE_D28, sizeof(field_d28));
	memcpy(&field_1344, bytes + ACTIVE_KIND, sizeof(field_1344));
	row->tag = tag;
	row->list1 = list1_keep(tag, field_1344, field_d28);
	row->gate2 = list2_gate(flag, tag, field_1344);
}

static void append_tag(void *game, uint64_t tag) {
	unsigned char *list = (unsigned char *)game + ACTIVE_LIST1;
	unsigned char *data = NULL;
	uint32_t cap = 0;
	uint32_t size = 0;
	uint32_t aux = 0;
	memcpy(&data, list + 8, sizeof(data));
	memcpy(&cap, list + 0x10, sizeof(cap));
	memcpy(&size, list + 0x14, sizeof(size));
	if (!data || size >= cap) return;
	memcpy(data + (size_t)size * sizeof(tag), &tag, sizeof(tag));
	size += 1;
	memcpy(list + 0x14, &size, sizeof(size));
	memcpy(&aux, (unsigned char *)game + ACTIVE_AUX_SIZE, sizeof(aux));
	aux += 1;
	memcpy((unsigned char *)game + ACTIVE_AUX_SIZE, &aux, sizeof(aux));
}

static void append_country(void *game, void *country) {
	unsigned char *list = (unsigned char *)game + ACTIVE_LIST2;
	unsigned char *end = NULL;
	unsigned char *cap = NULL;
	memcpy(&end, list + 8, sizeof(end));
	memcpy(&cap, list + 0x10, sizeof(cap));
	if (!end || !cap || end >= cap) return;
	memcpy(end, &country, sizeof(country));
	end += sizeof(country);
	memcpy(list + 8, &end, sizeof(end));
}

static uint32_t commit_active(void *game, void **countries, uint32_t start, struct active_decision *rows, uint64_t count) {
	uint64_t i;
	uint32_t sum = start;
	for (i = 0; i < count; ++i) {
		void *country = countries[(uint64_t)start + i];
		if (rows[i].list1) append_tag(game, rows[i].tag);
		if (rows[i].gate2 && active_pred && active_pred(country)) {
			append_country(game, country);
			sum += ACTIVE_SUM_STEP;
		}
	}
	return sum;
}

/* Spans of the last fixture-path takeover, in nanoseconds: judgment (pool
   handoff through the join, or the serial walk), write-back (predicate plus
   both list appends), and the whole takeover through the write-back. The
   decision-row and part allocations are inside the total; the proof write
   and the frees are outside every span. Only the calling thread writes or
   reads these. */
static long long active_judge_ns;
static long long active_writeback_ns;
static long long active_total_ns;
static int active_spans_valid;

static void store_active_spans(LARGE_INTEGER begin, LARGE_INTEGER judged, LARGE_INTEGER written) {
	active_judge_ns = qpc_ticks_to_ns(judged.QuadPart - begin.QuadPart);
	active_writeback_ns = qpc_ticks_to_ns(written.QuadPart - judged.QuadPart);
	active_total_ns = qpc_ticks_to_ns(written.QuadPart - begin.QuadPart);
	active_spans_valid = 1;
}

__declspec(dllexport) int foch_active_last_spans(long long *judge_ns, long long *writeback_ns, long long *total_ns) {
	if (!active_spans_valid) return 0;
	if (judge_ns) *judge_ns = active_judge_ns;
	if (writeback_ns) *writeback_ns = active_writeback_ns;
	if (total_ns) *total_ns = active_total_ns;
	return 1;
}

static void write_active_proof(uint32_t sum, struct active_decision *rows, uint64_t count) {
	DWORD ids[MAX_WORKERS];
	DWORD self = GetCurrentThreadId();
	int distinct = 0;
	int on_caller = 0;
	uint64_t i;
	char text[512];
	memset(ids, 0, sizeof(ids));
	for (i = 0; i < count; ++i) {
		int seen = 0;
		int j;
		if (rows[i].tid == 0 || rows[i].tid == self) {
			on_caller += 1;
			continue;
		}
		for (j = 0; j < distinct; ++j) if (ids[j] == rows[i].tid) seen = 1;
		if (!seen && distinct < MAX_WORKERS) ids[distinct++] = rows[i].tid;
	}
	snprintf(text, sizeof(text),
		"commit-thread=%lu\ncount=%llu\nsum=%u\nworker-count=%d\n"
		"worker-1=%lu\nworker-2=%lu\non-caller=%d\njudged=%llu\n"
		"judge-ns=%lld\nwriteback-ns=%lld\ntotal-ns=%lld\n",
		self, (unsigned long long)count, sum, distinct,
		distinct > 0 ? ids[0] : 0, distinct > 1 ? ids[1] : 0,
		on_caller, (unsigned long long)count,
		active_judge_ns, active_writeback_ns, active_total_ns);
	write_proof("active.txt", text);
}

static VOID CALLBACK run_active_part(PTP_CALLBACK_INSTANCE instance, PVOID param, PTP_WORK work) {
	struct active_part *part = param;
	uint64_t i;
	(void)instance;
	(void)work;
	for (i = part->from; i < part->to; ++i) {
		judge_country(part->countries[(uint64_t)part->start + i], part->flag, &part->rows[i]);
	}
}

static int ensure_active_pool(int workers) {
	if (active_pool_ready) return 1;
	active_pool = CreateThreadpool(NULL);
	active_group = active_pool ? CreateThreadpoolCleanupGroup() : NULL;
	if (!active_pool || !active_group) {
		if (active_group) CloseThreadpoolCleanupGroup(active_group);
		if (active_pool) CloseThreadpool(active_pool);
		active_group = NULL;
		active_pool = NULL;
		return 0;
	}
	InitializeThreadpoolEnvironment(&active_environment);
	SetThreadpoolThreadMaximum(active_pool, MAX_WORKERS);
	if (!SetThreadpoolThreadMinimum(active_pool, (DWORD)workers)) SetThreadpoolThreadMinimum(active_pool, 1);
	SetThreadpoolCallbackPool(&active_environment, active_pool);
	SetThreadpoolCallbackCleanupGroup(&active_environment, active_group, NULL);
	active_pool_ready = 1;
	return 1;
}

static void judge_serial(void **countries, uint32_t start, uint32_t flag, struct active_decision *rows, uint64_t count) {
	uint64_t i;
	for (i = 0; i < count; ++i) judge_country(countries[(uint64_t)start + i], flag, &rows[i]);
}

uint32_t active_takeover(void *game, uint32_t start, uint32_t flag) {
	struct active_decision *rows = NULL;
	struct active_part *parts = NULL;
	PTP_WORK *works = NULL;
	void **countries = NULL;
	void **end = NULL;
	LARGE_INTEGER t0, t1, t2;
	SYSTEM_INFO info;
	uint64_t total;
	uint64_t count;
	uint64_t cursor;
	uint32_t sum;
	int n;
	int i;
	int submitted = 0;
	active_enter_body = 0;
	if (active_real) {
		if (!game || !active_body) return start;
		memcpy(&countries, (unsigned char *)game + COUNTRY_ARRAY, sizeof(countries));
		memcpy(&end, (unsigned char *)game + COUNTRY_END, sizeof(end));
		if (!countries || !end || end < countries) return start;
		total = (uint64_t)(end - countries);
		if ((uint64_t)start >= total) return start;
		active_enter_body = 1;
		{
			LONG days = InterlockedIncrement(&active_real_days);
			if (days == 1 || days % 256 == 0) {
				char text[96];
				snprintf(text, sizeof(text), "days=%ld\n", days);
				write_proof("active-real.txt", text);
			}
		}
		return start;
	}
	if (!game) return start;
	memcpy(&countries, (unsigned char *)game + COUNTRY_ARRAY, sizeof(countries));
	memcpy(&end, (unsigned char *)game + COUNTRY_END, sizeof(end));
	if (!countries || !end || end < countries) return start;
	total = (uint64_t)(end - countries);
	if ((uint64_t)start >= total) return start;
	count = total - start;
	QueryPerformanceCounter(&t0);
	rows = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*rows) * (size_t)count);
	if (!rows) return start;
	GetSystemInfo(&info);
	n = (int)info.dwNumberOfProcessors;
	if (n < 2) n = 2;
	if ((uint64_t)n > count) n = (int)count;
	if (n > MAX_WORKERS) n = MAX_WORKERS;
	parts = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*parts) * (size_t)n);
	works = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, sizeof(*works) * (size_t)n);
	if (!parts || !works || n < 2 || !ensure_active_pool(n)) {
		judge_serial(countries, start, flag, rows, count);
		QueryPerformanceCounter(&t1);
		sum = commit_active(game, countries, start, rows, count);
		QueryPerformanceCounter(&t2);
		store_active_spans(t0, t1, t2);
		write_active_proof(sum, rows, count);
		goto done;
	}
	cursor = 0;
	for (i = 0; i < n; ++i) {
		uint64_t share = (count - cursor) / (uint64_t)(n - i);
		parts[i].countries = countries;
		parts[i].rows = rows;
		parts[i].start = start;
		parts[i].flag = flag;
		parts[i].from = cursor;
		parts[i].to = cursor + share;
		cursor += share;
		works[i] = CreateThreadpoolWork(run_active_part, &parts[i], &active_environment);
		if (!works[i]) run_active_part(NULL, &parts[i], NULL);
		else {
			SubmitThreadpoolWork(works[i]);
			submitted += 1;
		}
	}
	if (submitted) CloseThreadpoolCleanupGroupMembers(active_group, FALSE, NULL);
	QueryPerformanceCounter(&t1);
	sum = commit_active(game, countries, start, rows, count);
	QueryPerformanceCounter(&t2);
	store_active_spans(t0, t1, t2);
	write_active_proof(sum, rows, count);
done:
	if (parts) HeapFree(GetProcessHeap(), 0, parts);
	if (works) HeapFree(GetProcessHeap(), 0, works);
	HeapFree(GetProcessHeap(), 0, rows);
	return sum;
}

static int read_rip_i32(const FochHostApi *host, const unsigned char *insn, int32_t *out) {
	int32_t disp;
	int32_t value;
	void *target;
	if (!daily_a_readable(host->exe_base, host->exe_size, insn, 6)) return 0;
	if (insn[0] != 0x3B || insn[1] != 0x0D) return 0;
	memcpy(&disp, insn + 2, sizeof(disp));
	if (!daily_a_target(insn, 6, disp, &target) ||
		!daily_a_readable(host->exe_base, host->exe_size, target, sizeof(value))) return 0;
	memcpy(&value, target, sizeof(value));
	*out = value;
	return 1;
}

static void *find_active_resume(const FochHostApi *host, unsigned char *site) {
	void *resume = NULL;
	int hits = 0;
	size_t i;
	if (!daily_a_readable(host->exe_base, host->exe_size, site, 0x300)) return NULL;
	for (i = 0; i + 6 <= 0x300; ++i) {
		int32_t rel;
		unsigned char *insn = site + i;
		void *target;
		if (insn[0] != 0x0F || insn[1] != 0x8C) continue;
		memcpy(&rel, insn + 2, sizeof(rel));
		if (!daily_a_target(insn, 6, rel, &target) || target != site + 0x25) continue;
		resume = insn + 6;
		hits += 1;
	}
	return hits == 1 ? resume : NULL;
}

/* Installed loop body, site+0x25 through the back-edge. Hash 0x7747E17D. */
static const unsigned char active_body_image[] = {
	0x41,0x8B,0xC4,0x48,0x8D,0x34,0xC5,0x00,0x00,0x00,0x00,0x48,0x89,0x74,0x24,0x60,
	0x4C,0x8D,0x04,0x0E,0x4D,0x8B,0x30,0x49,0x8B,0x5E,0x20,0x48,0x8B,0xCB,0x48,0xC1,
	0xE9,0x30,0x48,0x8B,0xC3,0x48,0xC1,0xE8,0x30,0x3C,0x07,0x0F,0x84,0xCF,0x00,0x00,
	0x00,0x41,0x83,0xBE,0x44,0x13,0x00,0x00,0x00,0x75,0x35,0x80,0xF9,0x07,0x74,0x30,
	0x48,0x8B,0xC3,0x48,0xC1,0xE8,0x20,0x0F,0xBF,0xC8,0x3B,0x0D,0x04,0x2C,0xBE,0x01,
	0x74,0x1E,0x3B,0x0D,0x30,0x2C,0xBE,0x01,0x74,0x16,0x3B,0x0D,0x2C,0x2C,0xBE,0x01,
	0x74,0x0E,0x41,0x83,0xBE,0x28,0x0D,0x00,0x00,0x00,0x0F,0x84,0x90,0x00,0x00,0x00,
	0x41,0x8B,0x57,0x14,0xFF,0xC2,0x49,0x8B,0xCF,0xE8,0xA2,0xF1,0x96,0xFF,0x41,0x8B,
	0x45,0x14,0x44,0x8D,0x78,0x01,0x41,0x8B,0x75,0x10,0x44,0x3B,0xFE,0x7E,0x38,0x03,
	0xF6,0x44,0x3B,0xFE,0x41,0x0F,0x4F,0xF7,0x8B,0xCE,0xB8,0x08,0x00,0x00,0x00,0x48,
	0xF7,0xE1,0x48,0xC7,0xC1,0xFF,0xFF,0xFF,0xFF,0x48,0x0F,0x40,0xC1,0x48,0x8B,0xC8,
	0xE8,0x0B,0x86,0x2D,0x01,0x44,0x8B,0xC6,0x48,0x8B,0xD0,0x49,0x8B,0xCD,0xE8,0xDD,
	0x47,0x93,0xFF,0x41,0x8B,0x45,0x14,0x44,0x3B,0xF8,0x7E,0x24,0x48,0x98,0x49,0x63,
	0xD7,0x48,0x3B,0xC2,0x7D,0x1A,0x66,0x66,0x0F,0x1F,0x84,0x00,0x00,0x00,0x00,0x00,
	0x49,0x8B,0x4D,0x08,0x48,0x89,0x1C,0xC1,0x48,0xFF,0xC0,0x48,0x3B,0xC2,0x7C,0xF0,
	0x45,0x89,0x7D,0x14,0x48,0x8B,0x74,0x24,0x60,0x4C,0x8D,0xBD,0x00,0x1D,0x00,0x00,
	0x80,0x7C,0x24,0x68,0x00,0x0F,0x84,0x9D,0x00,0x00,0x00,0x41,0x83,0xBE,0x44,0x13,
	0x00,0x00,0x00,0x75,0x0B,0x41,0x80,0x7E,0x26,0x07,0x0F,0x85,0x88,0x00,0x00,0x00,
	0x49,0x8B,0x9E,0x20,0x14,0x00,0x00,0x48,0x85,0xDB,0x74,0x06,0x48,0x8B,0x43,0x10,
	0xEB,0x06,0xE8,0x39,0xE0,0x03,0x00,0x90,0x48,0x85,0xDB,0x74,0x06,0x48,0x83,0xC3,
	0x38,0xEB,0x14,0x48,0x85,0xC0,0x74,0x06,0x48,0x8D,0x58,0x78,0xEB,0x09,0xE8,0x1D,
	0xC6,0x04,0x00,0x48,0x8D,0x58,0x58,0x48,0x8B,0xCB,0xE8,0xF1,0x21,0x05,0x00,0x84,
	0xC0,0x75,0x0C,0x48,0x8B,0xCB,0xE8,0x05,0x21,0x05,0x00,0x84,0xC0,0x74,0x39,0x48,
	0x8D,0x8D,0x30,0x1D,0x00,0x00,0x4C,0x8B,0x85,0xE8,0x1C,0x00,0x00,0x4C,0x03,0xC6,
	0x48,0x8B,0x95,0x38,0x1D,0x00,0x00,0x48,0x3B,0x95,0x40,0x1D,0x00,0x00,0x74,0x0D,
	0x49,0x8B,0x00,0x48,0x89,0x02,0x48,0x83,0x41,0x08,0x08,0xEB,0x05,0xE8,0x2E,0x14,
	0x97,0xFF,0x81,0xC7,0xE8,0x03,0x00,0x00,0x41,0xFF,0xC4,0x48,0x8B,0x8D,0xE8,0x1C,
	0x00,0x00,0x48,0x8B,0x85,0xF0,0x1C,0x00,0x00,0x48,0x2B,0xC1,0x48,0xC1,0xF8,0x03,
	0x44,0x3B,0xE0,0x0F,0x8C,0x37,0xFE,0xFF,0xFF
};

static const uint32_t active_call_at[] = {
	0x9E, 0xD5, 0xE3, 0x157, 0x173, 0x17F, 0x18B, 0x1C2
};

#define ACTIVE_BODY_LO 0x25u
#define ACTIVE_BODY_HI 0x1EEu

static uint32_t body_hash(const unsigned char *bytes, size_t len) {
	uint32_t hash = 0;
	size_t i;
	for (i = 0; i < len; ++i) hash = hash * 131u + bytes[i];
	return hash;
}

static int disp_fits(ptrdiff_t delta) {
	return delta <= (ptrdiff_t)INT32_MAX && delta >= (ptrdiff_t)INT32_MIN;
}

static void *alloc_near(void *anchor, size_t bytes) {
	SYSTEM_INFO info;
	MEMORY_BASIC_INFORMATION region;
	uintptr_t granularity;
	uintptr_t origin;
	uintptr_t low;
	uintptr_t high;
	uintptr_t floor;
	uintptr_t cursor;
	int steps;
	GetSystemInfo(&info);
	granularity = (uintptr_t)info.dwAllocationGranularity;
	if (granularity == 0) granularity = 0x10000;
	origin = (uintptr_t)anchor;
	floor = (uintptr_t)info.lpMinimumApplicationAddress;
	low = origin > 0x7F000000u ? origin - 0x7F000000u : floor;
	if (low < floor) low = floor;
	high = origin > ~(uintptr_t)0 - 0x7F000000u ? ~(uintptr_t)0 : origin + 0x7F000000u;
	cursor = origin & ~(granularity - 1);
	for (steps = 0; cursor < high && steps < 512; ++steps) {
		uintptr_t next;
		uintptr_t start;
		if (!VirtualQuery((void *)cursor, &region, sizeof(region))) break;
		next = (uintptr_t)region.BaseAddress + region.RegionSize;
		if (region.State == MEM_FREE) {
			start = ((uintptr_t)region.BaseAddress + granularity - 1) & ~(granularity - 1);
			if (start >= low && start + bytes <= next && start + bytes >= start) {
				void *block = VirtualAlloc((void *)start, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
				if (block) return block;
			}
		}
		if (next <= cursor) break;
		cursor = next;
	}
	cursor = origin & ~(granularity - 1);
	for (steps = 0; cursor > low && steps < 512; ++steps) {
		uintptr_t next;
		uintptr_t start;
		uintptr_t end;
		if (!VirtualQuery((void *)(cursor - 1), &region, sizeof(region))) break;
		next = (uintptr_t)region.BaseAddress;
		end = next + region.RegionSize;
		if (region.State == MEM_FREE) {
			start = (next + granularity - 1) & ~(granularity - 1);
			if (start >= low && start + bytes <= end) {
				void *block = VirtualAlloc((void *)start, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
				if (block) return block;
			}
		}
		if (next >= cursor) break;
		cursor = next;
	}
	return NULL;
}

static int retarget(unsigned char *site, unsigned char *copy, uint32_t off, uint32_t insn_len, uint32_t disp_at) {
	int32_t rel;
	unsigned char *insn = site + off;
	unsigned char *moved = copy + (off - ACTIVE_BODY_LO);
	void *target;
	ptrdiff_t delta;
	if (disp_at > insn_len || sizeof(rel) > insn_len - disp_at || !memory_readable(insn, insn_len)) return 0;
	memcpy(&rel, insn + disp_at, sizeof(rel));
	if (!daily_a_target(insn, insn_len, rel, &target) || !memory_readable(target, disp_at == 2 ? 4 : 1)) return 0;
	delta = (ptrdiff_t)((int64_t)(uintptr_t)target - (int64_t)(uintptr_t)(moved + insn_len));
	if (!disp_fits(delta)) return 0;
	rel = (int32_t)delta;
	memcpy(moved + disp_at, &rel, sizeof(rel));
	return 1;
}

static void write_exit(unsigned char *at, void *ret) {
	at[0] = 0x8B;
	at[1] = 0xC7;
	at[2] = 0xFF;
	at[3] = 0x25;
	at[4] = 0;
	at[5] = 0;
	at[6] = 0;
	at[7] = 0;
	memcpy(at + 8, &ret, sizeof(ret));
}

static int active_profile_matches(const unsigned char *code, const unsigned char *profile, size_t bytes, int fixture) {
	size_t i;
	if (bytes != ACTIVE_BODY_HI - ACTIVE_BODY_LO || !memory_readable(code + ACTIVE_BODY_LO, bytes)) return 0;
	for (i = 0; i < bytes; ++i) {
		size_t off = i + ACTIVE_BODY_LO, call;
		int relocation = (off >= 0x71 && off < 0x75) || (off >= 0x79 && off < 0x7D) || (off >= 0x81 && off < 0x85);
		if (fixture) relocation |= off >= 0x158 && off < 0x15C;
		else for (call = 0; call < sizeof(active_call_at) / sizeof(active_call_at[0]); ++call)
			if (off > active_call_at[call] && off <= active_call_at[call] + 4) relocation = 1;
		if (!relocation && code[off] != profile[i]) return 0;
	}
	return 1;
}

static int active_body_is_real(const unsigned char *code) {
	return active_profile_matches(code, active_body_image, sizeof(active_body_image), 0);
}

static int active_body_is_fixture(const unsigned char *code) {
#ifdef FOCH_ACTIVE_FIXTURE_PROFILE
	return active_profile_matches(code, FOCH_ACTIVE_FIXTURE_PROFILE, sizeof(FOCH_ACTIVE_FIXTURE_PROFILE), 1);
#else
	(void)code;
	return 0;
#endif
}

static void *build_active_body(unsigned char *site, void *ret_label) {
	unsigned char *copy;
	uint32_t i;
	size_t body = ACTIVE_BODY_HI - ACTIVE_BODY_LO;
	int32_t rel;
	unsigned char *back;
	if (!active_body_is_real(site)) return NULL;
	copy = alloc_near(site, body + 16);
	if (!copy) return NULL;
	memcpy(copy, site + ACTIVE_BODY_LO, body);
	for (i = 0; i < sizeof(active_call_at) / sizeof(active_call_at[0]); ++i) {
		if (!retarget(site, copy, active_call_at[i], 5, 1)) {
			VirtualFree(copy, 0, MEM_RELEASE);
			return NULL;
		}
	}
	if (!retarget(site, copy, 0x6F, 6, 2) || !retarget(site, copy, 0x77, 6, 2) ||
		!retarget(site, copy, 0x7F, 6, 2)) {
		VirtualFree(copy, 0, MEM_RELEASE);
		return NULL;
	}
	back = copy + (0x1E8 - ACTIVE_BODY_LO);
	if (back[0] != 0x0F || back[1] != 0x8C) {
		VirtualFree(copy, 0, MEM_RELEASE);
		return NULL;
	}
	memcpy(&rel, back + 2, sizeof(rel));
	if (back + 6 + (ptrdiff_t)rel != copy) {
		VirtualFree(copy, 0, MEM_RELEASE);
		return NULL;
	}
	write_exit(copy + body, ret_label);
	if (!FlushInstructionCache(GetCurrentProcess(), copy, body + 16)) {
		VirtualFree(copy, 0, MEM_RELEASE);
		return NULL;
	}
	return copy;
}

static int point_disp(unsigned char *insn, uint32_t disp_at, uint32_t insn_len, void *target) {
	ptrdiff_t delta = (unsigned char *)target - (insn + insn_len);
	int32_t rel;
	if (!disp_fits(delta)) return 0;
	rel = (int32_t)delta;
	memcpy(insn + disp_at, &rel, sizeof(rel));
	return 1;
}

struct probe_country {
	unsigned char before_tag[0x20];
	uint64_t tag;
	unsigned char gap_d28[0xD00];
	uint32_t field_d28;
	unsigned char gap_kind[0x618];
	uint32_t field_1344;
	unsigned char gap_extra[0xD8];
	void *extra;
};

struct probe_world {
	unsigned char before_countries[0x1CE8];
	void **countries;
	void **countries_end;
	unsigned char pad_aux[8];
	unsigned char aux_before[0x14];
	uint32_t aux_size;
	void *list1_object;
	uint64_t *list1_data;
	uint32_t list1_cap;
	uint32_t list1_size;
	void **list2_begin;
	void **list2_end;
	void **list2_cap;
};

typedef char probe_tag_at[offsetof(struct probe_country, tag) == 0x20 ? 1 : -1];
typedef char probe_d28_at[offsetof(struct probe_country, field_d28) == 0xD28 ? 1 : -1];
typedef char probe_kind_at[offsetof(struct probe_country, field_1344) == 0x1344 ? 1 : -1];
typedef char probe_extra_at[offsetof(struct probe_country, extra) == 0x1420 ? 1 : -1];
typedef char probe_countries_at[offsetof(struct probe_world, countries) == 0x1CE8 ? 1 : -1];
typedef char probe_aux_at[offsetof(struct probe_world, aux_size) == 0x1D14 ? 1 : -1];
typedef char probe_list1_at[offsetof(struct probe_world, list1_object) == 0x1D18 ? 1 : -1];
typedef char probe_list1_size_at[offsetof(struct probe_world, list1_size) == 0x1D2C ? 1 : -1];
typedef char probe_list2_at[offsetof(struct probe_world, list2_begin) == 0x1D30 ? 1 : -1];
typedef char probe_list2_cap_at[offsetof(struct probe_world, list2_cap) == 0x1D40 ? 1 : -1];

static int probe_aux;
static int probe_query;
static int probe_fallback;
static int probe_keep;
static int probe_other;
static int probe_alloc;
static int probe_assign;
static int probe_grow;
static int32_t probe_g0 = 100;
static int32_t probe_g1 = 200;
static int32_t probe_g2 = 300;
static unsigned char probe_holder[0x20];
static unsigned char probe_fallback_obj[0x80];

static void stub_aux(void *obj, unsigned size) {
	uint32_t stored = size;
	memcpy((unsigned char *)obj + 0x14, &stored, sizeof(stored));
	probe_aux += 1;
}

static void *stub_alloc(uint64_t size) {
	(void)size;
	probe_alloc += 1;
	return NULL;
}

static void stub_assign(void *list, void *ptr, unsigned cap) {
	(void)list;
	(void)ptr;
	(void)cap;
	probe_assign += 1;
}

static void *stub_query(void) {
	probe_query += 1;
	return NULL;
}

static void *stub_fallback(void) {
	probe_fallback += 1;
	return probe_fallback_obj;
}

static int stub_keep(void *obj) {
	(void)obj;
	probe_keep += 1;
	return 1;
}

static int stub_other(void *obj) {
	(void)obj;
	probe_other += 1;
	return 0;
}

static void stub_grow(void *list, void *end, void *slot) {
	(void)list;
	(void)end;
	(void)slot;
	probe_grow += 1;
}

static void fill_probe_country(struct probe_country *row, unsigned index) {
	unsigned high = 1;
	unsigned mid = 50;
	uint32_t d28 = 0;
	uint32_t kind = 0;
	void *extra = probe_holder;
	memset(row, 0, sizeof(*row));
	switch (index) {
	case 0: high = 7; extra = NULL; break;
	case 1: kind = 1; break;
	case 2: mid = 100; break;
	case 4: d28 = 1; break;
	case 5: high = 7; kind = 1; extra = NULL; break;
	case 6: mid = 200; break;
	case 7: mid = 300; break;
	default: break;
	}
	row->tag = ((uint64_t)high << 48) | ((uint64_t)mid << 32) | index;
	row->field_d28 = d28;
	row->field_1344 = kind;
	row->extra = extra;
}

static void reset_probe_counts(void) {
	probe_aux = 0;
	probe_query = 0;
	probe_fallback = 0;
	probe_keep = 0;
	probe_other = 0;
	probe_alloc = 0;
	probe_assign = 0;
	probe_grow = 0;
}

static int probe_active_body(void) {
	static struct probe_world world;
	static struct probe_country rows[8];
	static void *slots[8];
	static uint64_t tags[8];
	static void *ptrs[8];
	static int ready = 0;
	unsigned char *site;
	void *copy;
	void *fns[8];
	uint32_t cmps[3];
	int32_t *globals[3];
	uint64_t tags_direct[8];
	void *ptrs_direct[8];
	uint32_t sum_direct = 0;
	uint32_t sum_copy = 0;
	uint32_t tag_n = 0;
	uint32_t ptr_n = 0;
	uint32_t hash;
	int aux_direct = 0;
	int query_direct = 0;
	int fallback_direct = 0;
	int keep_direct = 0;
	int other_direct = 0;
	int alloc_direct = 0;
	int assign_direct = 0;
	int grow_direct = 0;
	int match = 0;
	unsigned i;
	char text[640];
	hash = body_hash(active_body_image, sizeof(active_body_image));
	site = alloc_near((void *)stub_keep, 0x220);
	if (!ready) {
		memset(&world, 0, sizeof(world));
		for (i = 0; i < 8; ++i) {
			fill_probe_country(&rows[i], i);
			slots[i] = &rows[i];
		}
		world.countries = slots;
		world.countries_end = slots + 8;
		world.list1_data = tags;
		world.list1_cap = 8;
		world.list1_object = tags;
		world.list2_begin = ptrs;
		world.list2_end = ptrs;
		world.list2_cap = ptrs + 8;
		ready = 1;
	}
	if (site && hash == 0x7747E17Du && sizeof(active_body_image) == ACTIVE_BODY_HI - ACTIVE_BODY_LO) {
		memset(site, 0xCC, 0x220);
		memcpy(site + ACTIVE_BODY_LO, active_body_image, sizeof(active_body_image));
		fns[0] = (void *)stub_aux;
		fns[1] = (void *)stub_alloc;
		fns[2] = (void *)stub_assign;
		fns[3] = (void *)stub_query;
		fns[4] = (void *)stub_fallback;
		fns[5] = (void *)stub_keep;
		fns[6] = (void *)stub_other;
		fns[7] = (void *)stub_grow;
		cmps[0] = 0x6F;
		cmps[1] = 0x77;
		cmps[2] = 0x7F;
		globals[0] = &probe_g0;
		globals[1] = &probe_g1;
		globals[2] = &probe_g2;
		match = 1;
		for (i = 0; i < 8 && match; ++i) {
			site[active_call_at[i]] = 0xE8;
			if (!point_disp(site + active_call_at[i], 1, 5, fns[i])) match = 0;
		}
		for (i = 0; i < 3 && match; ++i) {
			site[cmps[i]] = 0x3B;
			site[cmps[i] + 1] = 0x0D;
			if (!point_disp(site + cmps[i], 2, 6, globals[i])) match = 0;
		}
	}
	copy = match ? build_active_body(site, (void *)active_invoke_back) : NULL;
	if (!copy) match = 0;
	if (match) {
		write_exit(site + ACTIVE_BODY_HI, (void *)active_invoke_back);
		FlushInstructionCache(GetCurrentProcess(), site, 0x220);
		world.aux_size = 0;
		world.list1_size = 0;
		world.list2_end = world.list2_begin;
		reset_probe_counts();
		sum_direct = active_invoke(site + ACTIVE_BODY_LO, &world, 1);
		tag_n = world.list1_size;
		ptr_n = tag_n <= 8 && world.list2_end >= world.list2_begin
			? (uint32_t)(world.list2_end - world.list2_begin) : 99u;
		if (tag_n > 8 || ptr_n > 8) match = 0;
		else {
			memcpy(tags_direct, tags, sizeof(tags_direct));
			memcpy(ptrs_direct, ptrs, sizeof(ptrs_direct));
			aux_direct = probe_aux;
			query_direct = probe_query;
			fallback_direct = probe_fallback;
			keep_direct = probe_keep;
			other_direct = probe_other;
			alloc_direct = probe_alloc;
			assign_direct = probe_assign;
			grow_direct = probe_grow;
			world.aux_size = 0;
			world.list1_size = 0;
			world.list2_end = world.list2_begin;
			reset_probe_counts();
			sum_copy = active_invoke(copy, &world, 1);
			if (sum_direct != 3000 || sum_copy != sum_direct) match = 0;
			if (tag_n != 5 || ptr_n != 3 || world.list1_size != tag_n) match = 0;
			if ((uint32_t)(world.list2_end - world.list2_begin) != ptr_n) match = 0;
			if (world.aux_size != tag_n) match = 0;
			if (memcmp(tags, tags_direct, tag_n * sizeof(tags[0])) != 0) match = 0;
			if (memcmp(ptrs, ptrs_direct, ptr_n * sizeof(ptrs[0])) != 0) match = 0;
			if (tags_direct[0] != (((uint64_t)1 << 48) | ((uint64_t)50 << 32) | 1)) match = 0;
			if (ptrs_direct[0] != &rows[0] || ptrs_direct[1] != &rows[1] || ptrs_direct[2] != &rows[5]) match = 0;
			if (aux_direct != 5 || query_direct != 2 || fallback_direct != 2 || keep_direct != 3) match = 0;
			if (other_direct != 0 || alloc_direct != 0 || assign_direct != 0 || grow_direct != 0) match = 0;
			if (probe_aux != aux_direct || probe_query != query_direct || probe_fallback != fallback_direct) match = 0;
			if (probe_keep != keep_direct || probe_other != other_direct) match = 0;
			if (probe_alloc != 0 || probe_assign != 0 || probe_grow != 0) match = 0;
		}
	} else {
		sum_direct = 0;
		sum_copy = 0;
		tag_n = 0;
		ptr_n = 0;
		aux_direct = query_direct = fallback_direct = keep_direct = 0;
		other_direct = alloc_direct = assign_direct = grow_direct = 0;
	}
	snprintf(text, sizeof(text),
		"match=%s\nsum=%u\ncopy-sum=%u\nbody-hash=%08x\n"
		"aux-calls=%d\nquery-calls=%d\nfallback-calls=%d\nkeep-calls=%d\n"
		"other-calls=%d\nalloc-calls=%d\nassign-calls=%d\ngrow-calls=%d\n"
		"tag-count=%u\nptr-count=%u\n",
		match ? "ok" : "bad", sum_direct, sum_copy, hash,
		aux_direct, query_direct, fallback_direct, keep_direct,
		other_direct, alloc_direct, assign_direct, grow_direct,
		tag_n, ptr_n);
	write_proof("active-trampoline.txt", text);
	return match ? 0 : -1;
}

static int32_t install_active(const FochHostApi *host, int *found, int *created, int *enabled, void **site_out) {
	static const char pattern[] = "44 8B E7 48 8B 8D E8 1C 00 00 48 8B 85 F0 1C 00 00";
	void *address = NULL;
	unsigned char *code;
	void *resume;
	int32_t rel = 0;
	uint32_t magic = 0;
	unsigned char *fn;
	*created = 0;
	*enabled = 0;
	*found = host->find_pattern(host, (FochStr){ pattern, sizeof(pattern) - 1 }, &address);
	*site_out = address;
	if (*found == FOCH_E_NOT_FOUND) return FOCH_OK;
	if (*found != FOCH_OK || !address) return *found == FOCH_OK ? FOCH_E_INVALID : *found;
	code = address;
	if (!daily_a_readable(host->exe_base, host->exe_size, code, 0x300)) return FOCH_E_INVALID;
	if (!read_rip_i32(host, code + 0x6F, &active_g0) || !read_rip_i32(host, code + 0x77, &active_g1) ||
		!read_rip_i32(host, code + 0x7F, &active_g2)) return FOCH_E_INVALID;
	resume = find_active_resume(host, code);
	if (!resume) return FOCH_E_NOT_FOUND;
	if (!image_code(host, resume)) return FOCH_E_INVALID;
	if (code[0x157] != 0xE8) return FOCH_E_INVALID;
	memcpy(&rel, code + 0x158, sizeof(rel));
	{
		void *target;
		if (!daily_a_target(code + 0x157, 5, rel, &target)) return FOCH_E_INVALID;
		fn = target;
	}
	if ((uintptr_t)fn < 4 || !image_code(host, fn) ||
		!daily_a_readable(host->exe_base, host->exe_size, (void *)((uintptr_t)fn - 4), sizeof(magic))) return FOCH_E_INVALID;
	memcpy(&magic, fn - 4, sizeof(magic));
	active_pred = NULL;
	active_body = NULL;
	active_real = 0;
	if (magic == 0xC0DEF00Du) {
		if (!active_header_is_known(code, 1) || !active_body_is_fixture(code)) return FOCH_E_INVALID;
		active_pred = (PredFn)fn;
	} else if (!active_header_is_known(code, 0) || !active_body_is_real(code)) {
		return FOCH_E_INVALID;
	} else {
		size_t call;
		for (call = 0; call < sizeof(active_call_at) / sizeof(active_call_at[0]); ++call) {
			void *target;
			memcpy(&rel, code + active_call_at[call] + 1, sizeof(rel));
			if (!daily_a_target(code + active_call_at[call], 5, rel, &target) || !image_code(host, target)) return FOCH_E_INVALID;
		}
		void *body = build_active_body(code, (void *)active_body_return);
		if (!body) return FOCH_E_MEMORY;
		active_body = body;
		active_real = 1;
	}
	active_site = address;
	active_after = resume;
	*created = owned_create(host, address, (void *)active_detour, 10, &active_next, &active_hook);
	if (*created != FOCH_OK) return *created;
	*enabled = owned_enable(host, active_hook);
	return *enabled;
}

static int32_t install_state(const FochHostApi *host, int *found, int *created, int *enabled, void **site) {
	static const char pattern[] =
		"40 53 55 56 57 41 54 41 55 41 56 41 57 48 83 EC 28 48 8B F9 45 33 ED";
	void *address = NULL;
	*created = 0;
	*enabled = 0;
	*found = host->find_pattern(host, (FochStr){ pattern, sizeof(pattern) - 1 }, &address);
	*site = address;
	if (*found == FOCH_E_NOT_FOUND) return FOCH_OK;
	if (*found != FOCH_OK || !address) return *found == FOCH_OK ? FOCH_E_INVALID : *found;
	*created = owned_create(host, address, (void *)state_detour, 10, &state_next, &state_hook);
	if (*created != FOCH_OK) return *created;
	*enabled = owned_enable(host, state_hook);
	if (*enabled != FOCH_OK) return *enabled;
	state_async = 1;
	return FOCH_OK;
}

/* ===== Daily loop A (0x759640): parallel queue drain, FOCH_DAILY_A gated =====
   The loop drains the country work queue at game+0x1e30 backward, one pass
   per day. Per country it either removes the entry and publishes {country}
   onto the intrusive list at game+0x1e40 (light path), or builds the war
   summary and then removes the entry through 0x77ac80 and publishes through
   0xdf120 (heavy path). Workers run a relocated copy of the body against a
   private snapshot of the queue plus a shadow frame holding a private
   header; the shared removals and publishes are recorded and replayed on
   the game thread in the original order, where they are exactly the serial
   operations. Heap traffic on workers goes to per-worker arenas through
   hooks on the two allocation funnels; the first day runs warm on one
   thread so the 0x8bf160 singleton initializes before any worker exists. */

#define DAILY_A_BODY_LEN 0x1441
#define DAILY_A_BODY_HASH 0xEEA14823u
#define DAILY_A_FIXTURE_HASH 0x05F61D14u
#define DAILY_A_STUBS_AT 0x1448
#define DA_OFF_COUNT1 0x4D
#define DA_OFF_COUNT2 0x81
#define DA_OFF_COUNT_STORE 0x8F
#define DA_OFF_REMOVE_BLOCK 0x97
#define DA_REMOVE_BLOCK_LEN 0x5C
#define DA_OFF_TAIL_DEC 0x415
#define DA_FIXTURE_OFF_TAIL_DEC 0x13C0
#define DA_OFF_SHIFT_CALL 0x137E
#define DA_OFF_PUBLISH_LEA 0x138A
#define DA_OFF_PUBLISH_CALL 0x1399
#define DA_OFF_AFTER_PUBLISH 0x139E
#define DA_OFF_JS 0x1433
#define DA_OFF_BACK_LEA 0x1435
#define DA_ALLOC_SITE 0xA4
#define DA_FREE_SITE 0x35F
#define DA_CTX_AT 0x1C0
#define DA_COUNT_AT 0x1D4
#define DA_SHADOW_RBP 0x300         /* rbp sits this far into the shadow block */

/* Fixture builds can fail the same reservation calls before native work. */
#ifndef DAILY_A_HEAP
#define DAILY_A_HEAP(kind, flags, bytes) HeapAlloc(GetProcessHeap(), flags, bytes)
#define DAILY_A_VIRTUAL(kind, address, bytes, flags, protect) VirtualAlloc(address, bytes, flags, protect)
#define DAILY_A_NATIVE(kind, bytes) daily_a_call_alloc(bytes)
#define DAILY_A_BODY(site) alloc_near(site, DAILY_A_STUBS_AT + 0xA0)
#endif

struct daily_a_ctx {          /* shadow rbp+0x1c0, mirrors game+0x1e28 */
	unsigned char pad0[8];     /* +0x00 */
	void **array;              /* +0x08 private queue snapshot */
	unsigned char pad1[4];     /* +0x10 */
	long count;                /* +0x14 private count */
};
typedef char daily_a_ctx_count_at_14[offsetof(struct daily_a_ctx, count) == 0x14 ? 1 : -1];

struct daily_a_arena {
	unsigned char *base;
	size_t reserved;
	size_t committed;
	size_t bump;
	uint64_t alloc_count;
	uint64_t free_count;
	uint64_t live_count;
	uint64_t peak_live;
	uint64_t oversized_fallbacks;
	void *freelist[62];        /* size classes 1..61, in 16-byte steps */
	SRWLOCK lock;              /* freelist pushes and accounting may come from any thread */
	unsigned char *extra_base; /* second span when a slice's budget exceeds 4MB */
	size_t extra_reserved;
	size_t extra_committed;
	size_t extra_bump;
};

struct daily_a_arena_totals {
	unsigned int arenas;
	uint64_t allocs;
	uint64_t frees;
	uint64_t live;
	uint64_t peak_live;
	uint64_t committed_bytes;
	uint64_t oversized_fallbacks;
};

struct daily_a_op {
	int kind;                  /* 1 remove+publish, 2 shift, 3 publish */
	unsigned int index;
	void *country;
};

struct daily_a_part {
	void **snapshot;           /* private queue copy */
	unsigned char *shadow;     /* rbp target; ctx at +0x1c0 */
	unsigned char *copy;       /* relocated body */
	void *game;
	uint64_t from;             /* lowest index of the slice */
	uint64_t to;               /* one past the highest index */
	unsigned long count;       /* full queue count for the count mirror */
	struct daily_a_op *ops;
	unsigned int op_count;
	unsigned int op_cap;
	struct daily_a_arena *arena;
	DWORD tid;
	int overflow;              /* a refused log; dispatch must not replay */
};

struct daily_a_publish_pool { void **nodes; size_t count, used; };
__declspec(thread) static struct daily_a_publish_pool *da_tls_publish;

static int daily_a_enabled;
static int daily_a_alloc_failed;
static int daily_a_stage;
static int daily_a_step;
static volatile LONG daily_a_calls;
static unsigned char *daily_a_body_copy;
static unsigned char *daily_a_site;
static unsigned char daily_a_head[15]; /* whole instructions replaced by the host redirect */
static uint32_t daily_a_site_hash;
static void *daily_a_image;
static size_t daily_a_image_size;
static DWORD daily_a_owner;
static void *daily_a_real_shift;    /* 0x77ac80 */
static void *daily_a_real_publish;  /* 0xdf120 */
static struct daily_a_arena *daily_a_arenas[MAX_WORKERS];
static int daily_a_arena_count;
static SRWLOCK daily_a_owner_lock = SRWLOCK_INIT;
__declspec(thread) static struct daily_a_arena *da_tls_arena;
__declspec(thread) static struct daily_a_part *da_tls_part;

extern void daily_a_detour(void);
extern void daily_a_end_detour(void);
extern void *const *daily_a_end_next;
extern void *daily_a_fallback_frame;
extern void daily_a_alloc_detour(void);
extern void daily_a_free_detour(void);
extern void daily_a_worker(struct daily_a_part *part);
extern void daily_a_worker_ret(void);
extern void *const *daily_a_next;
extern void *const *daily_a_alloc_next;
extern void *const *daily_a_free_next;
extern int daily_a_ready;             /* asm data cells */
extern void *daily_a_after;
extern volatile LONG daily_a_running;
extern void *fixture_daily_alloc_wrap_addr(void);
extern void *fixture_daily_free_wrap_addr(void);

/* Reading the chain cell at call time yields the original function before
   the redirect exists and the trampoline after, so routing never recurses. */
static void *daily_a_call_alloc(size_t size) {
	void *const *cell = daily_a_alloc_next;
	return ((void *(*)(size_t))(*cell))(size);
}

static void daily_a_call_free(void *ptr, size_t size) {
	void *const *cell = daily_a_free_next;
	((void (*)(void *, size_t))(*cell))(ptr, size);
}

static void *daily_a_arena_alloc(struct daily_a_arena *arena, size_t size) {
	size_t want;
	unsigned char *base;
	unsigned char *block;
	size_t klass;
	if (size == 0) size = 1;
	if (size > (sizeof(arena->freelist) / sizeof(arena->freelist[0]) - 1) * 16) {
		AcquireSRWLockExclusive(&arena->lock);
		arena->oversized_fallbacks += 1;
		ReleaseSRWLockExclusive(&arena->lock);
		return daily_a_call_alloc(size);
	}
	klass = (size + 15) / 16;
	AcquireSRWLockExclusive(&arena->lock);
	block = arena->freelist[klass];
	if (block) {
		arena->freelist[klass] = *(void **)(block + sizeof(size_t));
		arena->alloc_count += 1;
		arena->live_count += 1;
		if (arena->live_count > arena->peak_live) arena->peak_live = arena->live_count;
		ReleaseSRWLockExclusive(&arena->lock);
		return block + 16;
	}
	want = 16 + size;
	if (arena->bump + want > arena->committed) {
		size_t chunk = 0x10000;
		size_t need = arena->bump + want;
		size_t commit_offset = arena->committed;
		while (chunk < need - commit_offset) chunk *= 2;
		if (commit_offset + chunk > arena->reserved) {
			unsigned char *extra;
			if (arena->extra_base && arena->extra_bump + want <= arena->extra_committed) {
				extra = arena->extra_base + arena->extra_bump;
				arena->extra_bump += (want + 15) & ~(size_t)15;
				*(size_t *)extra = size;
				arena->alloc_count += 1;
				arena->live_count += 1;
				if (arena->live_count > arena->peak_live) arena->peak_live = arena->live_count;
				ReleaseSRWLockExclusive(&arena->lock);
				return extra + 16;
			}
			/* The reserved spans cannot hold this block. The native heap
			   returns the full size or NULL; the size is never clipped. */
			arena->oversized_fallbacks += 1;
			ReleaseSRWLockExclusive(&arena->lock);
			return daily_a_call_alloc(size);
		}
		ReleaseSRWLockExclusive(&arena->lock);
		if (!DAILY_A_VIRTUAL("arena-commit", arena->base + commit_offset, chunk, MEM_COMMIT, PAGE_READWRITE)) {
			return daily_a_call_alloc(size);
		}
		AcquireSRWLockExclusive(&arena->lock);
		/* Another worker may have committed the same span while the lock was
		   open; MEM_COMMIT is idempotent, so take the maximum and re-cover
		   this allocation before the bump advances past it. */
		if (arena->committed < commit_offset + chunk) arena->committed = commit_offset + chunk;
		if (arena->bump + want > arena->committed) {
			ReleaseSRWLockExclusive(&arena->lock);
			return daily_a_call_alloc(size);
		}
	}
	base = arena->base + arena->bump;
	arena->bump += (want + 15) & ~(size_t)15;
	*(size_t *)base = size;
	arena->alloc_count += 1;
	arena->live_count += 1;
	if (arena->live_count > arena->peak_live) arena->peak_live = arena->live_count;
	ReleaseSRWLockExclusive(&arena->lock);
	return base + 16;
}

static struct daily_a_arena *daily_a_arena_for(const void *ptr) {
	int i;
	uintptr_t at = (uintptr_t)ptr;
	struct daily_a_arena *found = NULL;
	AcquireSRWLockShared(&daily_a_owner_lock);
	for (i = 0; i < daily_a_arena_count; ++i) {
		struct daily_a_arena *arena = daily_a_arenas[i];
		if ((uintptr_t)arena->base <= at && at - (uintptr_t)arena->base < arena->reserved) { found = arena; break; }
		if (arena->extra_base && (uintptr_t)arena->extra_base <= at &&
			at - (uintptr_t)arena->extra_base < arena->extra_reserved) { found = arena; break; }
	}
	ReleaseSRWLockShared(&daily_a_owner_lock);
	return found;
}

static void daily_a_arena_free(void *ptr) {
	unsigned char *base;
	size_t size;
	size_t klass;
	struct daily_a_arena *arena = daily_a_arena_for(ptr);
	if (!arena) return;
	base = (unsigned char *)ptr - 16;
	size = *(size_t *)base;
	klass = (size + 15) / 16;
	if (klass >= 62) return;
	AcquireSRWLockExclusive(&arena->lock);
	*(void **)(base + sizeof(size_t)) = arena->freelist[klass];
	arena->freelist[klass] = base;
	arena->free_count += 1;
	if (arena->live_count > 0) arena->live_count -= 1;
	ReleaseSRWLockExclusive(&arena->lock);
}

static void daily_a_arena_totals_snapshot(struct daily_a_arena_totals *out) {
	int i;
	memset(out, 0, sizeof(*out));
	AcquireSRWLockShared(&daily_a_owner_lock);
	out->arenas = (unsigned int)daily_a_arena_count;
	for (i = 0; i < daily_a_arena_count; ++i) {
		struct daily_a_arena *arena = daily_a_arenas[i];
		AcquireSRWLockShared(&arena->lock);
		out->allocs += arena->alloc_count;
		out->frees += arena->free_count;
		out->live += arena->live_count;
		out->peak_live += arena->peak_live;
		out->committed_bytes += (uint64_t)arena->committed + (uint64_t)arena->extra_committed;
		out->oversized_fallbacks += arena->oversized_fallbacks;
		ReleaseSRWLockShared(&arena->lock);
	}
	ReleaseSRWLockShared(&daily_a_owner_lock);
}

/* Detour bodies: the routing only diverts worker threads. */
void *daily_a_route_alloc(size_t size) {
	if (da_tls_publish && size == 0x20) {
		if (da_tls_publish->used >= da_tls_publish->count || !da_tls_publish->nodes) return NULL;
		return da_tls_publish->nodes[da_tls_publish->used++];
	}
	if (da_tls_arena) return daily_a_arena_alloc(da_tls_arena, size);
	return daily_a_call_alloc(size);
}

void daily_a_route_free(void *ptr, size_t size) {
	if (daily_a_arena_for(ptr)) {
		daily_a_arena_free(ptr);
		return;
	}
	daily_a_call_free(ptr, size);
}

static void daily_a_log(int kind, unsigned int index, void *country) {
	struct daily_a_part *part = da_tls_part;
	if (!part) return;
	/* One light removal or two heavy operations per queue entry. The cap is
	   that bound for the slice. Past it, refuse the store: replaying a short
	   log would drop messages, and writing past it is an overflow. */
	if (part->op_count >= part->op_cap || !part->ops) {
		part->overflow = 1;
		return;
	}
	part->ops[part->op_count].kind = kind;
	part->ops[part->op_count].index = index;
	part->ops[part->op_count].country = country;
	part->op_count += 1;
}

/* Trampoline helpers called from the relocated copy. */
void daily_a_note_removal(void *country, unsigned int index) {
	daily_a_log(1, index, country);
}

void daily_a_note_shift(void *header, unsigned int index) {
	struct daily_a_ctx *ctx = header;
	void **array = ctx->array;
	long count = ctx->count;
	if ((long)index < count && count > 0) {
		memmove(&array[index], &array[index + 1], (size_t)(count - (long)index - 1) * sizeof(void *));
		ctx->count = count - 1;
	}
	daily_a_log(2, index, NULL);
}

void daily_a_note_publish(void *header, void *record) {
	(void)header;
	daily_a_log(3, 0, *(void **)record);
}

static int patch_bytes(unsigned char *at, const unsigned char *bytes, size_t len) {
	memcpy(at, bytes, len);
	return 1;
}

static void put_abs(unsigned char *at, const void *value) {
	memcpy(at, &value, 8);
}

static int put_rel32(unsigned char *at, size_t insn_len, const void *target) {
	ptrdiff_t delta = (const unsigned char *)target - (at + insn_len);
	int32_t rel;
	if (!disp_fits(delta)) return 0;
	rel = (int32_t)delta;
	memcpy(at + insn_len - sizeof(rel), &rel, sizeof(rel));
	return 1;
}

static void *daily_a_stub_at(unsigned char *copy, size_t stub_off) {
	return copy + DAILY_A_STUBS_AT + stub_off;
}

static int daily_a_readable(void *image, size_t image_size, const void *address, size_t bytes) {
	uintptr_t base = (uintptr_t)image, at = (uintptr_t)address;
	if (!image || image_size > UINTPTR_MAX - base || at < base || at - base > image_size ||
		bytes > image_size - (at - base)) return 0;
	return memory_readable(address, bytes);
}

static int daily_a_target(const unsigned char *insn, size_t len, int32_t rel, void **target) {
	uintptr_t next = (uintptr_t)insn, magnitude;
	if (len > UINTPTR_MAX - next) return 0;
	next += len;
	if (rel < 0) {
		magnitude = (uintptr_t)(-(int64_t)rel);
		if (magnitude > next) return 0;
		next -= magnitude;
	} else {
		if ((uintptr_t)rel > UINTPTR_MAX - next) return 0;
		next += (uintptr_t)rel;
	}
	*target = (void *)next;
	return 1;
}

/* Decode only understood x64 forms; unknown/VEX/address-size encodings
   are refused. All displacement reads come from the immutable source. */
struct daily_a_insn { size_t len, disp; int relative, width; };

static int daily_a_decode(const unsigned char *p, size_t n, struct daily_a_insn *out) {
	size_t i = 0, imm = 0;
	int word = 0, rex = 0, wide = 0, modrm = 0, group = 0;
	unsigned int op, reg;
	memset(out, 0, sizeof(*out));
	while (i < n && i < 15) {
		unsigned char b = p[i];
		if (b >= 0x40 && b <= 0x4F) {
			if (rex) return 0;
			rex = 1; wide = !!(b & 8); ++i;
		} else if (b == 0x66 || b == 0xF2 || b == 0xF3 || b == 0x2E || b == 0x3E || b == 0x64 || b == 0x65) {
			if (rex) return 0;
			if (b == 0x66) word = 1;
			++i;
		} else break;
	}
	if (i >= n || i >= 15 || (word && wide)) return 0;
	op = p[i++];
	if (op == 0xE8 || op == 0xE9) { out->relative = 1; out->disp = i; out->width = 4; imm = 4; }
	else if ((op >= 0x70 && op <= 0x7F) || op == 0xEB) { out->relative = 1; out->disp = i; out->width = 1; imm = 1; }
	else if (op == 0x0F) {
		if (i >= n) return 0;
		op = p[i++];
		if (op >= 0x80 && op <= 0x8F) { out->relative = 1; out->disp = i; out->width = 4; imm = 4; }
		else if ((op >= 0x10 && op <= 0x1F) || (op >= 0x28 && op <= 0x2F) ||
			(op >= 0x40 && op <= 0x4F) || (op >= 0x90 && op <= 0x9F) ||
			op == 0xAF || (op >= 0xB0 && op <= 0xB3) || op == 0xB6 || op == 0xB7 ||
			(op >= 0xBB && op <= 0xBF) || op == 0xC0 || op == 0xC1) modrm = 1;
		else if (op == 0x70 || (op >= 0x71 && op <= 0x73) || op == 0xBA || op == 0xC2 ||
			(op >= 0xC4 && op <= 0xC6)) { modrm = 1; imm = 1; }
		else return 0;
	} else if ((op >= 0x50 && op <= 0x5F) || op == 0x90 || op == 0x98 || op == 0x99 || op == 0xC3 || op == 0xCC) { }
	else if (op >= 0xB8 && op <= 0xBF) imm = wide ? 8 : word ? 2 : 4;
	else if (op == 0x68 || op == 0xA9 || (op <= 0x3D && (op & 7) == 5)) imm = word ? 2 : 4;
	else if (op == 0x6A || op == 0xA8 || (op >= 0xB0 && op <= 0xB7) || (op <= 0x3C && (op & 7) == 4)) imm = 1;
	else if ((op <= 0x3B && (op & 7) <= 3) || op == 0x63 || (op >= 0x84 && op <= 0x8F) ||
		(op >= 0xD0 && op <= 0xD3) || op == 0xFE || op == 0xFF) modrm = 1;
	else if (op == 0x69 || op == 0x81 || op == 0xC7) { modrm = 1; imm = word ? 2 : 4; if (op == 0xC7) group = 3; }
	else if (op == 0x6B || op == 0x80 || op == 0x83 || op == 0xC0 || op == 0xC1 || op == 0xC6) { modrm = 1; imm = 1; if (op == 0xC6) group = 3; }
	else if (op == 0xF6 || op == 0xF7) { modrm = 1; group = op == 0xF6 ? 1 : 2; }
	else return 0;
	if (modrm) {
		unsigned int mode, rm;
		if (i >= n) return 0;
		mode = p[i] >> 6; reg = (p[i] >> 3) & 7; rm = p[i++] & 7;
		if ((op == 0x8F && reg != 0) || (op == 0x8D && mode == 3) ||
			(op == 0xFE && reg > 1) || (op == 0xFF && (reg == 3 || reg == 5 || reg == 7)) ||
			((op == 0xF6 || op == 0xF7) && reg == 1) || (op == 0x1F && reg != 0)) return 0;
		if (group == 3 && reg != 0) return 0;
		if (group && group < 3 && reg <= 1) imm = group == 1 ? 1 : word ? 2 : 4;
		if (mode != 3) {
			if (rm == 4) {
				unsigned int sib;
				if (i >= n) return 0;
				sib = p[i++];
				if (mode == 0 && (sib & 7) == 5) i += 4;
			} else if (mode == 0 && rm == 5) { out->disp = i; out->width = 4; i += 4; }
			if (mode == 1) ++i;
			else if (mode == 2) i += 4;
		}
	}
	if (i > n || imm > n - i || i + imm > 15) return 0;
	out->len = i + imm;
	return 1;
}

static int daily_a_relocate(const unsigned char *original, unsigned char *copy, const unsigned char *site) {
	size_t at = 0;
	while (at < DA_OFF_JS) {
		struct daily_a_insn insn;
		int32_t old = 0, moved;
		int64_t delta, target;
		void *resolved;
		if (!daily_a_decode(original + at, DA_OFF_JS - at, &insn)) return 0;
		if (insn.width && !(at >= DA_OFF_REMOVE_BLOCK && at < DA_OFF_REMOVE_BLOCK + DA_REMOVE_BLOCK_LEN) &&
			at != DA_OFF_SHIFT_CALL && at != DA_OFF_PUBLISH_CALL) {
			if (insn.width == 1) old = (int8_t)original[at + insn.disp];
			else memcpy(&old, original + at + insn.disp, 4);
			if (!daily_a_target(site + at, insn.len, old, &resolved) ||
				!daily_a_readable(daily_a_image, daily_a_image_size, resolved, 1)) return 0;
			target = (int64_t)(uintptr_t)resolved;
			if ((uint64_t)target >= (uintptr_t)site && (uint64_t)target - (uintptr_t)site < DAILY_A_BODY_LEN)
				target = (int64_t)(uintptr_t)copy + (target - (int64_t)(uintptr_t)site);
			delta = target - (int64_t)(uintptr_t)(copy + at + insn.len);
			if (delta < INT32_MIN || delta > INT32_MAX || (insn.width == 1 && (delta < -128 || delta > 127))) return 0;
			moved = (int32_t)delta;
			memcpy(copy + at + insn.disp, &moved, (size_t)insn.width);
		}
		at += insn.len;
	}
	return at == DA_OFF_JS;
}

static unsigned char *build_daily_a_body(unsigned char *site, uint32_t expect_hash) {
	unsigned char *copy;
	unsigned char *stub;
	unsigned char original[DAILY_A_BODY_LEN];
	uint32_t hash;
	if (!daily_a_readable(daily_a_image, daily_a_image_size, site, DAILY_A_BODY_LEN + 0x20)) return NULL;
	hash = body_hash(site, DAILY_A_BODY_LEN);
	if (expect_hash && hash != expect_hash) return NULL; /* 0 = validated at install */
	int32_t rel;

	copy = DAILY_A_BODY(site);
	if (!copy) {
		daily_a_alloc_failed = 1;
		return NULL;
	}
	memcpy(original, site, sizeof(original));
	memcpy(original, daily_a_head, sizeof(daily_a_head));
	memcpy(copy, original, sizeof(original));
	if (!daily_a_relocate(original, copy, site)) goto fail;
	daily_a_stage = 1;
	/* P1/P2: mov r8d,[r12+0x1e3c] (8 bytes) -> mov r8d,[rbp+0x1d4] plus a nop */
	{
		unsigned char repl[] = {0x44, 0x8B, 0x85, 0xD4, 0x01, 0x00, 0x00, 0x90};
		unsigned char repl_store[] = {0x44, 0x89, 0x85, 0xD4, 0x01, 0x00, 0x00, 0x90};
		if (copy[DA_OFF_COUNT1] != 0x45 || copy[DA_OFF_COUNT1 + 1] != 0x8B) goto fail;
		patch_bytes(copy + DA_OFF_COUNT1, repl, sizeof(repl));
		if (copy[DA_OFF_COUNT2] != 0x45 || copy[DA_OFF_COUNT2 + 1] != 0x8B) goto fail;
		patch_bytes(copy + DA_OFF_COUNT2, repl, sizeof(repl));
		if (copy[DA_OFF_COUNT_STORE] != 0x45 || copy[DA_OFF_COUNT_STORE + 1] != 0x89) goto fail;
		patch_bytes(copy + DA_OFF_COUNT_STORE, repl_store, sizeof(repl_store));
	}
	daily_a_stage = 2;
	/* P4: removal+publish block -> jump to the removal stub. Every rel32 is
	   computed against the copy address, where the bytes actually run. */
	if (copy[DA_OFF_REMOVE_BLOCK] != 0x49 || copy[DA_OFF_REMOVE_BLOCK + 1] != 0x8B) goto fail;
	copy[DA_OFF_REMOVE_BLOCK] = 0xE9;
	if (!put_rel32(copy + DA_OFF_REMOVE_BLOCK, 5, daily_a_stub_at(copy, 0x00))) goto fail;
	memset(copy + DA_OFF_REMOVE_BLOCK + 5, 0xCC, DA_REMOVE_BLOCK_LEN - 5);
	daily_a_stage = 3;
	/* P5: call 0x77ac80 -> call the shift stub */
	if (copy[DA_OFF_SHIFT_CALL] != 0xE8) goto fail;
	if (!put_rel32(copy + DA_OFF_SHIFT_CALL, 5, daily_a_stub_at(copy, 0x30))) goto fail;
	daily_a_stage = 4;
	/* P6: the df120 call becomes a jump to the publish stub; both leas stay. */
	if (copy[DA_OFF_PUBLISH_LEA] != 0x49 || copy[DA_OFF_PUBLISH_LEA + 1] != 0x8D) goto fail;
	if (copy[DA_OFF_PUBLISH_CALL] != 0xE8) goto fail;
	copy[DA_OFF_PUBLISH_CALL] = 0xE9;
	if (!put_rel32(copy + DA_OFF_PUBLISH_CALL, 5, daily_a_stub_at(copy, 0x50))) goto fail;
	daily_a_stage = 5;
	/* P8 + P7: js -> worker exit (widened), lea rax,[r12+0x1e28] -> ctx, back-edge */
	if (copy[DA_OFF_JS] != 0x78) goto fail;
	if (copy[DA_OFF_BACK_LEA] != 0x49 || copy[DA_OFF_BACK_LEA + 1] != 0x8D) goto fail;
	if (copy[0x143D] != 0xE9) goto fail;
	copy[DA_OFF_JS] = 0x0F;
	copy[DA_OFF_JS + 1] = 0x88;
	if (!put_rel32(copy + DA_OFF_JS, 6, daily_a_stub_at(copy, 0x70))) goto fail;
	patch_bytes(copy + 0x1439, (unsigned char[]){0x48, 0x8D, 0x85, 0xC0, 0x01, 0x00, 0x00}, 7);
	copy[0x1440] = 0xE9;
	if (!put_rel32(copy + 0x1440, 5, daily_a_stub_at(copy, 0x80))) goto fail;
	daily_a_stage = 92;
	/* Stubs reach the plugin and the copy through absolute thunks
	   (mov rax,imm64; call/jmp rax): the plugin DLL may sit farther from
	   the copy than a rel32 can reach. */
	stub = copy + DAILY_A_STUBS_AT;
	{
		/* Removal note. Entry by jmp, rsp is 0 mod 16. */
		unsigned char *at = stub;
		at[0] = 0x48; at[1] = 0x83; at[2] = 0xEC; at[3] = 0x20;      /* sub rsp,0x20 */
		at[4] = 0x4C; at[5] = 0x89; at[6] = 0xF9;                   /* mov rcx,r15 */
		at[7] = 0x90; at[8] = 0x8B; at[9] = 0x95;                   /* nop; mov edx,[rbp+0xc8] */
		at[10] = 0xC8; at[11] = 0x00; at[12] = 0x00; at[13] = 0x00;
		at[14] = 0x48; at[15] = 0xB8;                               /* mov rax,note_removal */
		put_abs(at + 16, daily_a_note_removal);
		at[24] = 0xFF; at[25] = 0xD0;                               /* call rax */
		at[26] = 0x48; at[27] = 0x83; at[28] = 0xC4; at[29] = 0x20; /* add rsp,0x20 */
		at[30] = 0x48; at[31] = 0xB8;                               /* mov rax,tail_dec */
		put_abs(at + 32, copy + (daily_a_site_hash == DAILY_A_FIXTURE_HASH ?
			DA_FIXTURE_OFF_TAIL_DEC : DA_OFF_TAIL_DEC));
		at[40] = 0xFF; at[41] = 0xE0;                               /* jmp rax */
	}
	daily_a_stage = 93;
	stub = copy + DAILY_A_STUBS_AT + 0x30;
	{
		/* Shift note. Entry by call, rsp is 8 mod 16. */
		unsigned char *at = stub;
		at[0] = 0x48; at[1] = 0x83; at[2] = 0xEC; at[3] = 0x28;      /* sub rsp,0x28 */
		at[4] = 0x48; at[5] = 0xB8;                                  /* mov rax,note_shift */
		put_abs(at + 6, daily_a_note_shift);
		at[14] = 0xFF; at[15] = 0xD0;                               /* call rax */
		at[16] = 0x48; at[17] = 0x83; at[18] = 0xC4; at[19] = 0x28; /* add rsp,0x28 */
		at[20] = 0xC3;                                              /* ret */
	}
	daily_a_stage = 94;
	stub = copy + DAILY_A_STUBS_AT + 0x50;
	{
		/* Publish note. Entry by jmp, rsp is 0 mod 16. */
		unsigned char *at = stub;
		at[0] = 0x48; at[1] = 0x83; at[2] = 0xEC; at[3] = 0x20;      /* sub rsp,0x20 */
		at[4] = 0x48; at[5] = 0xB8;                                  /* mov rax,note_publish */
		put_abs(at + 6, daily_a_note_publish);
		at[14] = 0xFF; at[15] = 0xD0;                               /* call rax */
		at[16] = 0x48; at[17] = 0x83; at[18] = 0xC4; at[19] = 0x20; /* add rsp,0x20 */
		at[20] = 0x48; at[21] = 0xB8;                                /* mov rax,after_publish */
		put_abs(at + 22, copy + DA_OFF_AFTER_PUBLISH);
		at[30] = 0xFF; at[31] = 0xE0;                               /* jmp rax */
	}
	/* Stub 0x70: worker exit through the asm return label. */
	stub = copy + DAILY_A_STUBS_AT + 0x70;
	write_exit(stub, daily_a_worker_ret);
	/* The native exit tests index < 0. A worker must also stop below its
	   assigned lower index, while keeping the native index/count arithmetic. */
	stub = copy + DAILY_A_STUBS_AT + 0x80;
	patch_bytes(stub, (unsigned char[]){0x48,0x3B,0x8D,0xE0,0x01,0,0,0x0F,0x8C}, 9);
	if (!put_rel32(stub + 7, 6, daily_a_stub_at(copy, 0x70))) goto fail;
	stub[13] = 0xE9;
	if (!put_rel32(stub + 13, 5, copy)) goto fail;
	/* The fixture wires these calls after plugin init. Resolve the replay
	   targets with the same finalized source bytes used for this copy. */
	memcpy(&rel, site + DA_OFF_SHIFT_CALL + 1, sizeof(rel));
	if (!daily_a_target(site + DA_OFF_SHIFT_CALL, 5, rel, &daily_a_real_shift) ||
		!daily_a_readable(daily_a_image, daily_a_image_size, daily_a_real_shift, 1)) goto fail;
	memcpy(&rel, site + DA_OFF_PUBLISH_CALL + 1, sizeof(rel));
	if (!daily_a_target(site + DA_OFF_PUBLISH_CALL, 5, rel, &daily_a_real_publish) ||
		!daily_a_readable(daily_a_image, daily_a_image_size, daily_a_real_publish, 1)) goto fail;
	if (!FlushInstructionCache(GetCurrentProcess(), copy, DAILY_A_STUBS_AT + 0xA0)) goto fail;
	return copy;
fail:
	VirtualFree(copy, 0, MEM_RELEASE);
	if (daily_a_alloc_failed == 0) daily_a_alloc_failed = 0xB0 + daily_a_stage;
	return NULL;
}

static struct daily_a_arena *daily_a_arena_new(void) {
	SYSTEM_INFO info;
	struct daily_a_arena *arena;
	void *base;
	/* Every returned owner is registered for its entire process lifetime. */
	if (daily_a_arena_count >= MAX_WORKERS) return NULL;
	GetSystemInfo(&info);
	arena = DAILY_A_HEAP("arena", HEAP_ZERO_MEMORY, sizeof(*arena));
	if (!arena) return NULL;
	base = DAILY_A_VIRTUAL("arena-reserve", NULL, 0x400000, MEM_RESERVE, PAGE_READWRITE);
	if (!base) {
		HeapFree(GetProcessHeap(), 0, arena);
		return NULL;
	}
	arena->base = base;
	arena->reserved = 0x400000;
	InitializeSRWLock(&arena->lock);
	AcquireSRWLockExclusive(&daily_a_owner_lock);
	daily_a_arenas[daily_a_arena_count++] = arena;
	ReleaseSRWLockExclusive(&daily_a_owner_lock);
	return arena;
}

static struct daily_a_arena *daily_a_arena_get(int index) {
	/* Dispatch is serialized; keep owners stable across days, including the
	   warm owner at index zero. Never reset storage with live game pointers. */
	if (index < 0 || index >= MAX_WORKERS || index > daily_a_arena_count) return NULL;
	if (index == daily_a_arena_count) return daily_a_arena_new();
	return daily_a_arenas[index];
}

static int daily_a_arena_prepare(struct daily_a_arena *arena) {
	if (arena->committed == arena->reserved) return 1;
	if (!DAILY_A_VIRTUAL("arena-commit", arena->base + arena->committed,
		arena->reserved - arena->committed, MEM_COMMIT, PAGE_READWRITE)) return 0;
	AcquireSRWLockExclusive(&arena->lock);
	arena->committed = arena->reserved;
	ReleaseSRWLockExclusive(&arena->lock);
	return 1;
}

static void daily_a_run_part(struct daily_a_part *part) {
	part->tid = GetCurrentThreadId();
	da_tls_part = part;
	da_tls_arena = part->arena;
	daily_a_worker(part);
	da_tls_arena = NULL;
	da_tls_part = NULL;
}

static VOID CALLBACK daily_a_work(PTP_CALLBACK_INSTANCE instance, PVOID param, PTP_WORK work) {
	(void)instance;
	(void)work;
	daily_a_run_part(param);
}

static int daily_a_replay(void *game, struct daily_a_part *parts, int n, struct daily_a_publish_pool *pool) {
	typedef void (*ShiftFn)(void *, unsigned int);
	typedef void (*PublishFn)(void *, void *);
	unsigned char *g = game;
	int s;
	int o;
	/* Serial order drains the highest indices first, and parts[0] holds the
	   highest slice, so the replay walks the parts in ascending order. */
	for (s = 0; s < n; ++s) {
		struct daily_a_part *part = &parts[s];
		for (o = 0; o < (int)part->op_count; ++o) {
			struct daily_a_op *op = &part->ops[o];
			if (op->kind == 1) {
				void **array = NULL;
				void *node;
				void *tail = NULL;
				long count = 0;
				long node_count = 0;
				long lower;
				memcpy(&array, g + 0x1E30, sizeof(array));
				memcpy(&count, g + 0x1E3C, sizeof(count));
				if ((long)op->index < count && count > 0) {
					memmove(&array[op->index], &array[op->index + 1],
						(size_t)(count - (long)op->index - 1) * sizeof(void *));
					lower = count - 1;
					memcpy(g + 0x1E3C, &lower, sizeof(lower));
				}
				if (pool->used >= pool->count || !pool->nodes) return 0;
				node = pool->nodes[pool->used++];
				((void **)node)[0] = op->country;
				memcpy(&tail, g + 0x1E48, sizeof(tail));
				((void **)node)[1] = tail;
				((void **)node)[2] = NULL;
				((unsigned char *)node)[0x18] = 0;
				memcpy(&node_count, g + 0x1E50, sizeof(node_count));
				if (node_count) ((void **)tail)[2] = node;
				else memcpy(g + 0x1E40, &node, sizeof(node));
				memcpy(g + 0x1E48, &node, sizeof(node));
				node_count += 1;
				memcpy(g + 0x1E50, &node_count, sizeof(node_count));
			} else if (op->kind == 2) {
				((ShiftFn)daily_a_real_shift)(g + 0x1E28, op->index);
			} else if (op->kind == 3) {
				da_tls_publish = pool;
				((PublishFn)daily_a_real_publish)(g + 0x1E40, &op->country);
				da_tls_publish = NULL;
			}
		}
	}
	return 1;
}

static int daily_mul_size(size_t count, size_t each, size_t *out) {
	if (each != 0 && count > SIZE_MAX / each) return 0;
	*out = count * each;
	return 1;
}

/* Bytes of simultaneous arena traffic charged to one queue entry. A slice
   whose product does not fit the worker's reserved spans grows a second
   span; if that cannot be committed, the dispatch does not start. */
#define DAILY_ENTRY_BUDGET 64u

static int daily_a_arena_ensure(struct daily_a_arena *arena, size_t bytes) {
	size_t have, extra;
	void *block;
	if (!arena) return 0;
	have = arena->reserved + arena->extra_reserved;
	if (bytes <= have) return 1;
	if (arena->extra_base) return 0;
	extra = bytes - have;
	block = DAILY_A_VIRTUAL("arena-grow", NULL, extra, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
	if (!block) return 0;
	AcquireSRWLockExclusive(&arena->lock);
	if (arena->extra_base) {
		ReleaseSRWLockExclusive(&arena->lock);
		VirtualFree(block, 0, MEM_RELEASE);
		return 0;
	}
	arena->extra_reserved = extra;
	arena->extra_committed = extra;
	arena->extra_bump = 0;
	MemoryBarrier();
	arena->extra_base = (unsigned char *)block;
	ReleaseSRWLockExclusive(&arena->lock);
	return 1;
}

void daily_a_finish_fallback(void *frame) {
	if (daily_a_running && daily_a_fallback_frame == frame && daily_a_owner == GetCurrentThreadId()) {
		daily_a_fallback_frame = NULL;
		daily_a_owner = 0;
		InterlockedExchange(&daily_a_running, 0);
	}
}

int daily_a_dispatch(void *game) {
	unsigned char *g = game;
	void **array = NULL;
	long count = 0;
	long long t0 = qpc_ticks_now();
	struct daily_a_part *parts = NULL;
	struct daily_a_publish_pool pool = {0};
	SYSTEM_INFO info;
	int n = 0, i, completed = 0;
	LONG call;
	long long span;
	char text[512];
	size_t snap_bytes = 0, op_bytes = 0, budget = 0;
	daily_a_owner = GetCurrentThreadId();
	memcpy(&array, g + 0x1E30, sizeof(array));
	memcpy(&count, g + 0x1E3C, sizeof(count));
	if (!array || count <= 0 || (uint64_t)count > UINT_MAX / 2) goto fallback;
	if (!daily_mul_size((size_t)count, sizeof(void *), &snap_bytes) ||
		!daily_mul_size((size_t)count, 2 * sizeof(struct daily_a_op), &op_bytes) ||
		!daily_mul_size((size_t)count, DAILY_ENTRY_BUDGET, &budget) ||
		op_bytes < (size_t)count) goto fallback;
	if (!daily_a_body_copy) {
		daily_a_body_copy = build_daily_a_body(daily_a_site, 0);
		if (!daily_a_body_copy) goto fallback;
	}
	GetSystemInfo(&info);
	n = (int)info.dwNumberOfProcessors;
	if (n < 1) n = 1;
	if (n > MAX_WORKERS) n = MAX_WORKERS;
	if ((long)n > count) n = (int)count;
	if (daily_a_calls == 0) n = 1;
	else if (!ensure_active_pool(n)) n = 1;
	parts = DAILY_A_HEAP("parts", HEAP_ZERO_MEMORY, sizeof(*parts) * (size_t)n);
	if (!parts) goto fallback;
	pool.nodes = DAILY_A_HEAP("publish-table", HEAP_ZERO_MEMORY, snap_bytes);
	if (!pool.nodes) goto fallback;
	for (i = 0; i < count; ++i) {
		void *node = DAILY_A_NATIVE("publish-node", 0x20);
		if (!node) goto fallback;
		pool.nodes[pool.count++] = node;
	}
	{
		long cursor = count;
		for (i = 0; i < n; ++i) {
			long share = cursor / (n - i);
			parts[i].game = game;
			parts[i].count = (unsigned long)count;
			parts[i].copy = daily_a_body_copy;
			parts[i].from = (uint64_t)(cursor - share);
			parts[i].to = (uint64_t)cursor;
			if ((uint64_t)share > UINT_MAX / 2) goto fallback;
			parts[i].op_cap = (unsigned int)share * 2;
			cursor -= share;
		}
	}
	/* No country body runs until every slice's complete log, frame and arena,
	   and every replay publication node, have been reserved successfully. */
	for (i = 0; i < n; ++i) {
		parts[i].snapshot = DAILY_A_HEAP("snapshot", 0, snap_bytes);
		parts[i].shadow = DAILY_A_VIRTUAL("shadow", NULL, 0x800, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
		parts[i].ops = DAILY_A_HEAP("ops", HEAP_ZERO_MEMORY, sizeof(*parts[i].ops) * parts[i].op_cap);
		parts[i].arena = daily_a_arena_get(i);
		if (!parts[i].snapshot || !parts[i].shadow || !parts[i].ops || !parts[i].arena ||
			!daily_a_arena_ensure(parts[i].arena, budget) ||
			!daily_a_arena_prepare(parts[i].arena)) goto fallback;
		memcpy(parts[i].snapshot, array, sizeof(void *) * (size_t)count);
		memcpy(parts[i].shadow + DA_SHADOW_RBP + DA_CTX_AT + 0x08, &parts[i].snapshot, sizeof(void *));
		memcpy(parts[i].shadow + DA_SHADOW_RBP + DA_COUNT_AT, &count, sizeof(count));
	}
	call = InterlockedIncrement(&daily_a_calls);
	write_proof("daily-a-dispatch.txt", "stage=entered\n");
	if (n == 1) {
		daily_a_run_part(&parts[0]);
	} else {
		PTP_WORK works[MAX_WORKERS];
		int submitted = 0;
		for (i = 0; i < n; ++i) {
			works[i] = CreateThreadpoolWork(daily_a_work, &parts[i], &active_environment);
			if (!works[i]) daily_a_run_part(&parts[i]);
			else { SubmitThreadpoolWork(works[i]); ++submitted; }
		}
		if (submitted) CloseThreadpoolCleanupGroupMembers(active_group, FALSE, NULL);
	}
	for (i = 0; i < n; ++i) if (parts[i].overflow) goto fallback;
	{
		void **undo = DAILY_A_HEAP("undo", 0, snap_bytes);
		void *saved_head = NULL, *saved_tail = NULL;
		long saved_nodes = 0;
		if (!undo) goto fallback;
		memcpy(undo, array, snap_bytes);
		memcpy(&saved_head, g + 0x1E40, sizeof(saved_head));
		memcpy(&saved_tail, g + 0x1E48, sizeof(saved_tail));
		memcpy(&saved_nodes, g + 0x1E50, sizeof(saved_nodes));
		if (!daily_a_replay(game, parts, n, &pool)) {
			memcpy(array, undo, snap_bytes);
			memcpy(g + 0x1E3C, &count, sizeof(count));
			memcpy(g + 0x1E40, &saved_head, sizeof(saved_head));
			memcpy(g + 0x1E48, &saved_tail, sizeof(saved_tail));
			memcpy(g + 0x1E50, &saved_nodes, sizeof(saved_nodes));
			pool.used = 0;
			HeapFree(GetProcessHeap(), 0, undo);
			goto fallback;
		}
		HeapFree(GetProcessHeap(), 0, undo);
	}
	span = qpc_ticks_to_ns(qpc_ticks_now() - t0);
	{
		unsigned int total_ops = 0;
		struct daily_a_arena_totals totals;
		for (i = 0; i < n; ++i) total_ops += parts[i].op_count;
		daily_a_arena_totals_snapshot(&totals);
		snprintf(text, sizeof(text),
			"calls=%ld\ncountries=%ld\nworkers=%d\nops=%u\nspan-ns=%lld\n"
			"arena-count=%u\narena-allocs=%llu\narena-frees=%llu\n"
			"arena-live=%llu\narena-peak-live=%llu\n"
			"arena-committed-bytes=%llu\narena-oversized=%llu\n",
			call, count, n, total_ops, span,
			totals.arenas,
			(unsigned long long)totals.allocs,
			(unsigned long long)totals.frees,
			(unsigned long long)totals.live,
			(unsigned long long)totals.peak_live,
			(unsigned long long)totals.committed_bytes,
			(unsigned long long)totals.oversized_fallbacks);
		write_proof("daily-a-real.txt", text);
	}
	completed = 1;
	goto cleanup;
fallback:
	write_proof("daily-a-dispatch.txt", "stage=preflight-fallback\n");
cleanup:
	if (parts) {
		for (i = 0; i < n; ++i) {
			if (parts[i].snapshot) HeapFree(GetProcessHeap(), 0, parts[i].snapshot);
			if (parts[i].shadow) VirtualFree(parts[i].shadow, 0, MEM_RELEASE);
			if (parts[i].ops) HeapFree(GetProcessHeap(), 0, parts[i].ops);
		}
		HeapFree(GetProcessHeap(), 0, parts);
	}
	while (pool.used < pool.count) daily_a_call_free(pool.nodes[pool.used++], 0x20);
	if (pool.nodes) HeapFree(GetProcessHeap(), 0, pool.nodes);
	return completed;
}

static int daily_a_install_started;

static int32_t install_daily_a(const FochHostApi *host, int *found, void **site_out) {
	static const char pattern[] = "48 8B 50 08 4C 8B 3C CA 4C 89 BD A8 00 00 00 4C 89 BD F8 00 00 00";
	void *address = NULL;
	unsigned char *site;
	uint32_t hash;
	int32_t rel = 0;
	FochHook *hook = NULL;
	FochHook *alloc_hook = NULL;
	FochHook *free_hook = NULL;
	FochHook *end_hook = NULL;
	*found = host->find_pattern(host, (FochStr){ pattern, sizeof(pattern) - 1 }, &address);
	*site_out = address;
	if (*found != FOCH_OK || !address) return FOCH_OK;
	site = address;
	if (!daily_a_readable(host->exe_base, host->exe_size, site, DAILY_A_BODY_LEN + 0x20)) return FOCH_E_INVALID;
	/* The allocation-free fallback gateway must match the original frame. */
	{
		static const unsigned char frame[] = {
			0x48,0x89,0x4C,0x24,0x08,0x55,0x53,0x56,0x57,0x41,0x54,0x41,0x55,0x41,0x56,0x41,
			0x57,0x48,0x8D,0xAC,0x24,0xF8,0xFE,0xFF,0xFF,0x48,0x81,0xEC,0x08,0x02,0,0};
		if ((uintptr_t)site < 0x60 || !daily_a_readable(host->exe_base, host->exe_size, site - 0x60, sizeof(frame)) ||
			memcmp(site - 0x60, frame, sizeof(frame))) return FOCH_E_INVALID;
		{
			static const unsigned char end[] = {0x48,0x81,0xC4,0x08,0x02,0,0,0x41,0x5F,0x41,0x5E,0x41,0x5D,0x41,0x5C,0x5F,0x5E,0x5B,0x5D,0xC3};
			if (memcmp(site + 0x1442, end, sizeof(end))) return FOCH_E_INVALID;
		}
	}
	hash = body_hash(site, DAILY_A_BODY_LEN);
	{
		if (hash != DAILY_A_BODY_HASH && hash != DAILY_A_FIXTURE_HASH) return FOCH_E_INVALID;
		/* On the real game the takeover stays behind FOCH_DAILY_A; the
		   fixture body installs unconditionally for the differential. */
		if (hash == DAILY_A_BODY_HASH && !daily_a_enabled) return FOCH_OK;
	}
	/* The copy is built on first dispatch: the fixture wires its calls in
	 * main, after this init, and the copy bakes their targets verbatim. */
	daily_a_site = site;
	daily_a_image = host->exe_base;
	daily_a_image_size = host->exe_size;
	daily_a_site_hash = hash;
	memcpy(daily_a_head, site, sizeof(daily_a_head));
	daily_a_after = site + 0x1442;
	/* The funnel targets: on the real game the body's calls name them and
	   frees route through a jmp thunk; the fixture exports its wrappers,
	   because its calls are wired in main, after this init. */
	void *alloc_target;
	void *free_target;
	if (hash == DAILY_A_BODY_HASH) {
		if (site[DA_ALLOC_SITE] != 0xE8 || site[DA_FREE_SITE] != 0xE8) return FOCH_E_INVALID;
		memcpy(&rel, site + DA_ALLOC_SITE + 1, sizeof(rel));
		if (!daily_a_target(site + DA_ALLOC_SITE, 5, rel, &alloc_target)) return FOCH_E_INVALID;
		memcpy(&rel, site + DA_FREE_SITE + 1, sizeof(rel));
		if (!daily_a_target(site + DA_FREE_SITE, 5, rel, &free_target)) return FOCH_E_INVALID;
		if (!daily_a_readable(host->exe_base, host->exe_size, free_target, 5) || *(unsigned char *)free_target != 0xE9) return FOCH_E_INVALID;
		memcpy(&rel, (unsigned char *)free_target + 1, sizeof(rel));
		if (!daily_a_target(free_target, 5, rel, &free_target)) return FOCH_E_INVALID;
	} else {
		HMODULE exe = GetModuleHandleA(NULL);
		typedef void *(*AddrFn)(void);
		union { FARPROC proc; AddrFn fn; } a, f;
		a.proc = GetProcAddress(exe, "fixture_daily_alloc_wrap_addr");
		f.proc = GetProcAddress(exe, "fixture_daily_free_wrap_addr");
		if (!a.proc || !f.proc) { daily_a_step = 7; return FOCH_E_INVALID; }
		alloc_target = a.fn();
		free_target = f.fn();
	}
	if (!daily_a_readable(host->exe_base, host->exe_size, alloc_target, 15) ||
		!daily_a_readable(host->exe_base, host->exe_size, free_target, 15)) return FOCH_E_INVALID;
	daily_a_install_started = 1;
	{
		int32_t rc;
		rc = owned_create(host, alloc_target, (void *)daily_a_alloc_detour, 10, &daily_a_alloc_next, &alloc_hook);
		if (rc != FOCH_OK) { daily_a_step = 1; return rc; }
		rc = owned_create(host, free_target, (void *)daily_a_free_detour, 10, &daily_a_free_next, &free_hook);
		if (rc != FOCH_OK) { daily_a_step = 2; return rc; }
		rc = owned_create(host, daily_a_after, (void *)daily_a_end_detour, 10, &daily_a_end_next, &end_hook);
		if (rc != FOCH_OK) { daily_a_step = 8; return rc; }
		rc = owned_create(host, address, (void *)daily_a_detour, 10, &daily_a_next, &hook);
		if (rc != FOCH_OK) { daily_a_step = 3; return rc; }
		rc = owned_enable(host, end_hook);
		if (rc != FOCH_OK) { daily_a_step = 9; return rc; }
		rc = owned_enable(host, hook);
		if (rc != FOCH_OK) { daily_a_step = 4; return rc; }
		rc = owned_enable(host, alloc_hook);
		if (rc != FOCH_OK) { daily_a_step = 5; return rc; }
		rc = owned_enable(host, free_hook);
		if (rc != FOCH_OK) { daily_a_step = 6; return rc; }
	}
	daily_a_ready = 1;
	return FOCH_OK;
}

/* ===== Daily loop B (0xa89360): two walks, FOCH_DAILY_B gated =====
   Walk 1 moves backward over revolution centers. Walk 2 moves forward over
   spread targets and reads walk 1's flags, so it starts only after walk 1's
   log is complete. Workers mutate a private center snapshot and record
   effects; the game thread replays that log in element order. Reservation
   failure or a log that would pass its cap leaves the world untouched and
   the original body runs instead.
   The installed 0xa89360 body stays native. 0x93d750's own stores are
   stack slots; its only immediate 1 is mov bl,1 on the assert path, and it
   never writes the state dword at this+8. The four sites in 0xa1a9a0 pass
   country_table[tag_index]+0x28c0, with the index taken from the current
   object's tag at +0x468, so the object is not proven to be that object.
   When the dword is already 1 the function is the CALCULATING re-entry
   assert and calls 0x14d6110, whose fast path writes the caller's buffer
   pointer and three dwords through fixed RIP slots on every entry.
   The next daily-chain callee after 0x759640 and 0xa89360 is 0x1d0e20
   (called from 0x75c895, 22 back-edges, one pointer-array walk). Its body
   has no RIP store, but callee 0xd405b0 writes 1 through RIP bytes, so the
   walk is not taken over. FOCH_DAILY_B=1 hooks only the fixture stand-in. */

#define DB_ZEAL 1
#define DB_QUIET 2
#define DB_FLAG 3
#define DB_MOD 4
#define DB_AUTO 5
#define DB_FIRED 6
#define DB_SAME 7
#define DB_MSG 8
#define DB_SPREAD 9
#define DB_THRESHOLD 0x186a0u

struct db_center {
	uint32_t zeal;
	uint8_t quiet;
	uint8_t pad[3];
	uint32_t country;
};
struct db_country {
	uint32_t id;
	uint32_t tag;
	uint8_t flag;
	uint8_t pad[3];
	int32_t modifier_acc;
	int32_t autonomy_acc;
};
struct db_province {
	uint32_t country;
	uint32_t id;
	uint8_t gate;
	uint8_t pad[3];
};
struct db_world {
	struct db_center *centers;
	int32_t center_count;
	struct db_country *countries;
	int32_t country_count;
	struct db_province *provinces;
	int32_t province_count;
	int32_t *fired;
	int32_t fired_count;
	int32_t fired_cap;
	int32_t *same_tag;
	int32_t same_count;
	int32_t same_cap;
	int32_t *spread_id;
	int32_t *spread_n;
	int32_t spread_count;
	int32_t spread_cap;
	int32_t *messages;
	int32_t message_count;
	int32_t message_cap;
	int32_t message_factor;
	uint32_t player_tag;
	uint32_t zeal_step;
	int32_t singleton;
	int32_t quiet_count;
};
struct db_op { int kind; int32_t a; int32_t b; };
struct db_part {
	struct db_world *src;
	struct db_op *ops;
	unsigned op_count, op_cap;
	int from, to, walk, overflow;
	struct daily_a_arena *arena;
	const uint8_t *flags;
	const int32_t *fired;
	int32_t fired_n;
};

static int daily_b_enabled, daily_b_ready, daily_b_install_started;
static volatile LONG daily_b_running, daily_b_calls;
static void *const *daily_b_next;
static FochHook *daily_b_hook;
static void *daily_b_real_site;

static int db_world_ok(const struct db_world *w) {
	if (!w || w->center_count < 0 || w->country_count < 0 || w->province_count < 0 || w->message_factor < 0) return 0;
	if (w->center_count && !w->centers) return 0;
	if (w->country_count && !w->countries) return 0;
	if (w->province_count && !w->provinces) return 0;
	return 1;
}

static int db_grow(int32_t **buf, int32_t *cap, int32_t need) {
	int32_t *next;
	size_t bytes;
	if (need <= *cap) return 1;
	if (need < 0 || !daily_mul_size((size_t)need, sizeof(int32_t), &bytes)) return 0;
	next = DAILY_A_HEAP("db-result", HEAP_ZERO_MEMORY, bytes);
	if (!next) return 0;
	if (*buf && *cap > 0) memcpy(next, *buf, (size_t)(*cap) * sizeof(int32_t));
	if (*buf) HeapFree(GetProcessHeap(), 0, *buf);
	*buf = next;
	*cap = need;
	return 1;
}

static int db_prepare(struct db_world *w) {
	int32_t messages = 0;
	if (w->message_factor != 0 && w->center_count > INT32_MAX / w->message_factor) return 0;
	messages = w->center_count * w->message_factor;
	if (!db_grow(&w->fired, &w->fired_cap, w->center_count) ||
		!db_grow(&w->same_tag, &w->same_cap, w->center_count) ||
		!db_grow(&w->messages, &w->message_cap, messages) ||
		!db_grow(&w->spread_id, &w->spread_cap, w->province_count)) return 0;
	{
		int32_t ncap = 0, *nbuf = NULL;
		if (!db_grow(&nbuf, &ncap, w->province_count)) return 0;
		if (w->spread_n && w->spread_n != nbuf) HeapFree(GetProcessHeap(), 0, w->spread_n);
		w->spread_n = nbuf;
	}
	return 1;
}

static int db_apply_one(struct db_world *w, int kind, int32_t a, int32_t b) {
	switch (kind) {
	case DB_ZEAL:
		if (a < 0 || a >= w->center_count) return 0;
		w->centers[a].zeal = (uint32_t)b;
		return 1;
	case DB_QUIET:
		w->quiet_count += a;
		return 1;
	case DB_FLAG:
		if (a < 0 || a >= w->country_count) return 0;
		w->countries[a].flag = (uint8_t)(w->countries[a].flag | (uint8_t)b);
		return 1;
	case DB_MOD:
		if (a < 0 || a >= w->country_count) return 0;
		w->countries[a].modifier_acc += b;
		return 1;
	case DB_AUTO:
		if (a < 0 || a >= w->country_count) return 0;
		w->countries[a].autonomy_acc += b;
		return 1;
	case DB_FIRED:
		if (w->fired_count >= w->fired_cap) return 0;
		w->fired[w->fired_count++] = a;
		return 1;
	case DB_SAME:
		if (w->same_count >= w->same_cap) return 0;
		w->same_tag[w->same_count++] = a;
		return 1;
	case DB_MSG:
		if (w->message_count >= w->message_cap) return 0;
		w->messages[w->message_count++] = a;
		return 1;
	case DB_SPREAD:
		if (w->spread_count >= w->spread_cap) return 0;
		w->spread_id[w->spread_count] = a;
		w->spread_n[w->spread_count] = b;
		w->spread_count += 1;
		return 1;
	default:
		return 0;
	}
}

static int db_emit(struct db_world *direct, struct db_part *part, int kind, int32_t a, int32_t b) {
	if (direct) return db_apply_one(direct, kind, a, b);
	if (!part || part->op_count >= part->op_cap || !part->ops) {
		if (part) part->overflow = 1;
		return 0;
	}
	part->ops[part->op_count].kind = kind;
	part->ops[part->op_count].a = a;
	part->ops[part->op_count].b = b;
	part->op_count += 1;
	return 1;
}

static int db_scratch(struct db_part *part) {
	void *block = daily_a_route_alloc(32);
	if (!block) {
		if (part) part->overflow = 1;
		return 0;
	}
	memset(block, 0x5A, 32);
	daily_a_route_free(block, 32);
	return 1;
}

static void db_warmup(struct db_world *w) {
	void *block;
	if (w->singleton) return;
	block = daily_a_route_alloc(32);
	if (block) {
		memset(block, 1, 32);
		daily_a_route_free(block, 32);
	}
	w->singleton = 1;
}

static int db_walk1(struct db_part *part, struct db_world *direct, struct db_world *src, int index) {
	struct db_center *center;
	struct db_country *row;
	uint32_t zeal, country;
	int32_t modifier, autonomy;
	int message;
	if (index < 0 || index >= src->center_count) return 0;
	center = &src->centers[index];
	zeal = center->zeal + src->zeal_step;
	center->zeal = zeal;
	if (!db_emit(direct, part, DB_ZEAL, index, (int32_t)zeal)) return 0;
	if (zeal < DB_THRESHOLD) {
		if (center->quiet == 0 && !db_emit(direct, part, DB_QUIET, 1, 0)) return 0;
		return 1;
	}
	if (!db_scratch(part) || !db_scratch(part)) return 0;
	country = center->country;
	if (country >= (uint32_t)src->country_count) {
		if (part) part->overflow = 1;
		return 0;
	}
	row = &src->countries[country];
	modifier = (int32_t)(zeal ^ row->id);
	autonomy = (int32_t)(row->tag * 3u);
	if (!db_emit(direct, part, DB_FLAG, (int32_t)country, 1) ||
		!db_emit(direct, part, DB_MOD, (int32_t)country, modifier) ||
		!db_emit(direct, part, DB_AUTO, (int32_t)country, autonomy) ||
		!db_emit(direct, part, DB_FIRED, index, (int32_t)row->id)) return 0;
	if (row->tag == src->player_tag && !db_emit(direct, part, DB_SAME, (int32_t)row->id, 0)) return 0;
	for (message = 0; message < src->message_factor; ++message) {
		long long id = (long long)index * src->message_factor + message;
		if (id > INT32_MAX || !db_emit(direct, part, DB_MSG, (int32_t)id, 0)) {
			if (part) part->overflow = 1;
			return 0;
		}
	}
	return 1;
}

static int db_spread_blocked(const struct db_world *w, const int32_t *fired, int32_t fired_n, uint32_t province_id) {
	int32_t i;
	for (i = 0; i < fired_n; ++i) {
		int32_t center = fired[i];
		uint32_t country;
		if (center < 0 || center >= w->center_count) continue;
		country = w->centers[center].country;
		if (country < (uint32_t)w->country_count && w->countries[country].id == province_id) return 1;
	}
	return 0;
}

static int db_walk2(struct db_part *part, struct db_world *direct, struct db_world *src, int index,
		const uint8_t *flags, const int32_t *fired, int32_t fired_n) {
	struct db_province *province;
	uint32_t country;
	int count, flagged;
	if (index < 0 || index >= src->province_count) return 0;
	province = &src->provinces[index];
	country = province->country;
	if (country >= (uint32_t)src->country_count) return 1;
	flagged = flags ? (flags[country] & 1) != 0 : (src->countries[country].flag & 1) != 0;
	if (!flagged || (province->gate & 0x10) == 0) return 1;
	if (db_spread_blocked(src, fired, fired_n, province->id)) return 1;
	count = (int)(province->id % 5u);
	if (count <= 0) return 1;
	if (!db_scratch(part)) return 0;
	return db_emit(direct, part, DB_SPREAD, (int32_t)province->id, count);
}

static VOID CALLBACK db_work(PTP_CALLBACK_INSTANCE instance, PVOID param, PTP_WORK work) {
	struct db_part *part = param;
	int index;
	(void)instance;
	(void)work;
	da_tls_part = NULL;
	da_tls_arena = part->arena;
	if (part->walk == 1) {
		for (index = part->to - 1; index >= part->from; --index)
			if (!db_walk1(part, NULL, part->src, index)) break;
	} else {
		for (index = part->from; index < part->to; ++index)
			if (!db_walk2(part, NULL, part->src, index, part->flags, part->fired, part->fired_n)) break;
	}
	da_tls_arena = NULL;
}

static int db_ops_for(int count, int each, unsigned *out) {
	if (count < 0 || each < 1 || (unsigned)count > UINT_MAX / (unsigned)each) return 0;
	*out = (unsigned)count * (unsigned)each;
	return 1;
}

static int db_slice(struct db_world *src, int walk, int high_first, const uint8_t *flags,
		const int32_t *fired, int32_t fired_n, int ops_each, struct db_part **out_parts, int *out_n) {
	SYSTEM_INFO info;
	struct db_part *parts;
	int count = walk == 1 ? src->center_count : src->province_count;
	int n, i, cursor;
	size_t budget = 0;
	*out_parts = NULL;
	*out_n = 0;
	if (count == 0) return 1;
	GetSystemInfo(&info);
	n = (int)info.dwNumberOfProcessors;
	if (n < 1) n = 1;
	if (n > MAX_WORKERS) n = MAX_WORKERS;
	if (n > count) n = count;
	if (daily_b_calls == 0) n = 1;
	else if (!ensure_active_pool(n)) n = 1;
	parts = DAILY_A_HEAP("db-parts", HEAP_ZERO_MEMORY, sizeof(*parts) * (size_t)n);
	if (!parts) return 0;
	if (!daily_mul_size((size_t)count, DAILY_ENTRY_BUDGET, &budget)) {
		HeapFree(GetProcessHeap(), 0, parts);
		return 0;
	}
	cursor = high_first ? count : 0;
	for (i = 0; i < n; ++i) {
		int share = high_first ? cursor / (n - i) : (count - cursor) / (n - i);
		unsigned cap = 0;
		parts[i].src = src;
		parts[i].walk = walk;
		parts[i].flags = flags;
		parts[i].fired = fired;
		parts[i].fired_n = fired_n;
		parts[i].from = high_first ? cursor - share : cursor;
		parts[i].to = high_first ? cursor : cursor + share;
		cursor = high_first ? parts[i].from : parts[i].to;
		if (!db_ops_for(share, ops_each, &cap)) {
			int k;
			for (k = 0; k < i; ++k) if (parts[k].ops) HeapFree(GetProcessHeap(), 0, parts[k].ops);
			HeapFree(GetProcessHeap(), 0, parts);
			return 0;
		}
		parts[i].op_cap = cap;
		parts[i].ops = cap ? DAILY_A_HEAP("db-ops", HEAP_ZERO_MEMORY, sizeof(struct db_op) * cap) : NULL;
		parts[i].arena = daily_a_arena_get(i);
		if ((cap && !parts[i].ops) || !parts[i].arena ||
			!daily_a_arena_ensure(parts[i].arena, budget) ||
			!daily_a_arena_prepare(parts[i].arena)) {
			int k;
			for (k = 0; k <= i; ++k) if (parts[k].ops) HeapFree(GetProcessHeap(), 0, parts[k].ops);
			HeapFree(GetProcessHeap(), 0, parts);
			return 0;
		}
	}
	if (n == 1) db_work(NULL, &parts[0], NULL);
	else {
		PTP_WORK works[MAX_WORKERS];
		int submitted = 0;
		for (i = 0; i < n; ++i) {
			works[i] = CreateThreadpoolWork(db_work, &parts[i], &active_environment);
			if (!works[i]) db_work(NULL, &parts[i], NULL);
			else { SubmitThreadpoolWork(works[i]); ++submitted; }
		}
		if (submitted) CloseThreadpoolCleanupGroupMembers(active_group, FALSE, NULL);
	}
	*out_parts = parts;
	*out_n = n;
	return 1;
}

static int db_overflow(struct db_part *parts, int n) {
	int i;
	for (i = 0; i < n; ++i) if (parts[i].overflow) return 1;
	return 0;
}

static void db_release(struct db_part *parts, int n) {
	int i;
	if (!parts) return;
	for (i = 0; i < n; ++i) if (parts[i].ops) HeapFree(GetProcessHeap(), 0, parts[i].ops);
	HeapFree(GetProcessHeap(), 0, parts);
}

static int db_apply_parts(struct db_world *w, struct db_part *parts, int n) {
	int s, o;
	for (s = 0; s < n; ++s)
		for (o = 0; o < (int)parts[s].op_count; ++o)
			if (!db_apply_one(w, parts[s].ops[o].kind, parts[s].ops[o].a, parts[s].ops[o].b)) return 0;
	return 1;
}

__declspec(dllexport) unsigned daily_b_layout(void) {
	return (unsigned)sizeof(struct db_world) ^ 0x0B170000u;
}

/* The 0xa89360 prologue. install_daily_b records it and does not hook it. */
static const unsigned char daily_b_real_prologue[] = {
	0x48,0x89,0x4C,0x24,0x08,0x55,0x53,0x56,0x57,0x41,0x54,0x41,0x55,0x41,0x56,
	0x41,0x57,0x48,0x8D,0xAC,0x24,0x28,0xEC,0xFF,0xFF
};

__declspec(dllexport) int daily_b_real_prologue_blocked(const unsigned char *bytes, size_t len) {
	if (!bytes || len < sizeof daily_b_real_prologue) return 0;
	return memcmp(bytes, daily_b_real_prologue, sizeof daily_b_real_prologue) == 0;
}

__declspec(dllexport) void daily_b_serial(void *opaque) {
	struct db_world *w = opaque;
	int index;
	if (!db_world_ok(w) || !db_prepare(w)) return;
	db_warmup(w);
	for (index = w->center_count - 1; index >= 0; --index) db_walk1(NULL, w, w, index);
	for (index = 0; index < w->province_count; ++index)
		db_walk2(NULL, w, w, index, NULL, w->fired, w->fired_count);
}

static int daily_b_dispatch(struct db_world *w) {
	struct db_center *copy = NULL;
	struct db_world snap;
	struct db_part *walk1 = NULL, *walk2 = NULL;
	uint8_t *flags = NULL;
	int32_t *fired = NULL, fired_n = 0;
	int n1 = 0, n2 = 0, i, o, ops_each, centers;
	size_t bytes = 0;
	unsigned ops = 0;
	char text[512];
	struct daily_a_arena_totals totals;
	if (!db_world_ok(w) || !db_prepare(w)) return 0;
	if (w->message_factor > INT32_MAX - 6) return 0;
	ops_each = 6 + w->message_factor;
	centers = w->center_count;
	if (centers && (!daily_mul_size((size_t)centers, sizeof(*copy), &bytes) ||
		!(copy = DAILY_A_HEAP("db-snap", 0, bytes)))) return 0;
	if (centers) memcpy(copy, w->centers, bytes);
	snap = *w;
	snap.centers = copy ? copy : w->centers;
	if (!db_slice(&snap, 1, 1, NULL, NULL, 0, ops_each, &walk1, &n1) || db_overflow(walk1, n1)) goto abandon;
	flags = centers ? DAILY_A_HEAP("db-flags", HEAP_ZERO_MEMORY, (size_t)w->country_count) : NULL;
	fired = centers ? DAILY_A_HEAP("db-fired", 0, sizeof(int32_t) * (size_t)centers) : NULL;
	if (centers && (!flags || !fired)) goto abandon;
	for (i = 0; i < n1; ++i) {
		for (o = 0; o < (int)walk1[i].op_count; ++o) {
			struct db_op *op = &walk1[i].ops[o];
			if (op->kind == DB_FLAG && op->a >= 0 && op->a < w->country_count) flags[op->a] = (uint8_t)(flags[op->a] | 1);
			else if (op->kind == DB_FIRED && fired_n < centers) fired[fired_n++] = op->a;
		}
	}
	if (!db_slice(w, 2, 0, flags, fired, fired_n, 1, &walk2, &n2) || db_overflow(walk2, n2)) goto abandon;
	db_warmup(w);
	if (!db_apply_parts(w, walk1, n1) || !db_apply_parts(w, walk2, n2)) goto abandon;
	for (i = 0; i < n1; ++i) ops += walk1[i].op_count;
	for (i = 0; i < n2; ++i) ops += walk2[i].op_count;
	InterlockedIncrement(&daily_b_calls);
	daily_a_arena_totals_snapshot(&totals);
	snprintf(text, sizeof(text),
		"calls=%ld\ncenters=%d\nprovinces=%d\nfactor=%d\nworkers=%d\nops=%u\n"
		"arena-allocs=%llu\narena-frees=%llu\narena-live=%llu\narena-oversized=%llu\n",
		daily_b_calls, w->center_count, w->province_count, w->message_factor, n1 > n2 ? n1 : n2, ops,
		(unsigned long long)totals.allocs, (unsigned long long)totals.frees,
		(unsigned long long)totals.live, (unsigned long long)totals.oversized_fallbacks);
	write_proof("daily-b-real.txt", text);
	db_release(walk1, n1);
	db_release(walk2, n2);
	if (copy) HeapFree(GetProcessHeap(), 0, copy);
	if (flags) HeapFree(GetProcessHeap(), 0, flags);
	if (fired) HeapFree(GetProcessHeap(), 0, fired);
	return 1;
abandon:
	db_release(walk1, n1);
	db_release(walk2, n2);
	if (copy) HeapFree(GetProcessHeap(), 0, copy);
	if (flags) HeapFree(GetProcessHeap(), 0, flags);
	if (fired) HeapFree(GetProcessHeap(), 0, fired);
	write_proof("daily-b-dispatch.txt", "stage=preflight-fallback\n");
	return 0;
}

static void daily_b_detour(void *world) {
	void (*native)(void *);
	if (!daily_b_next || !*daily_b_next) return;
	native = (void (*)(void *))(*daily_b_next);
	if (InterlockedCompareExchange(&daily_b_running, 1, 0) != 0) {
		native(world);
		return;
	}
	if (!daily_b_ready || !daily_b_dispatch(world)) native(world);
	InterlockedExchange(&daily_b_running, 0);
}

static int32_t install_daily_b(const FochHostApi *host, int *found, void **site_out) {
	static const char pattern[] = "48 89 4C 24 08 55 53 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 28 EC FF FF";
	HMODULE exe;
	void *site = NULL, *real_site = NULL;
	int32_t rc;
	*found = 0;
	*site_out = NULL;
	daily_b_real_site = NULL;
	if (host->find_pattern(host, (FochStr){ pattern, sizeof(pattern) - 1 }, &real_site) == FOCH_OK)
		daily_b_real_site = real_site;
	if (!daily_b_enabled) return FOCH_OK;
	exe = GetModuleHandleA(NULL);
	{
		typedef void *(*AddrFn)(void);
		AddrFn addr = exe ? (AddrFn)GetProcAddress(exe, "fixture_daily_b_addr") : NULL;
		site = addr ? addr() : NULL;
	}
	if (!site) return FOCH_OK;
	*found = 1;
	*site_out = site;
	if (!daily_a_readable(host->exe_base, host->exe_size, site, 16)) return FOCH_E_INVALID;
	daily_b_install_started = 1;
	rc = owned_create(host, site, (void *)daily_b_detour, 10, &daily_b_next, &daily_b_hook);
	if (rc != FOCH_OK) return rc;
	rc = owned_enable(host, daily_b_hook);
	if (rc != FOCH_OK) return rc;
	daily_b_ready = 1;
	return FOCH_OK;
}

static void *build_monthly_loop(const FochHostApi *host, const unsigned char *site) {
	unsigned char *copy;
	unsigned calls[2] = { 0x13, 0x3A };
	unsigned count, i;
	if (!monthly_profile_is_known(host, site)) return NULL;
	count = monthly_bytes_match(site, monthly_image, sizeof(monthly_image)) ? 2 : 1;
	copy = alloc_near((void *)site, sizeof(monthly_image) + 14);
	if (!copy) return NULL;
	memcpy(copy, site, sizeof(monthly_image));
	for (i = 0; i < count; ++i) {
		int32_t rel;
		void *target;
		memcpy(&rel, site + calls[i] + 1, sizeof(rel));
		uintptr_t next = (uintptr_t)(copy + calls[i] + 5), destination;
		if (site[calls[i]] != 0xE8 || !daily_a_target(site + calls[i], 5, rel, &target) ||
			!image_code(host, target)) {
			VirtualFree(copy, 0, MEM_RELEASE);
			return NULL;
		}
		destination = (uintptr_t)target;
		if (destination >= next ? destination - next > INT32_MAX : next - destination > (uint64_t)INT32_MAX + 1) {
			VirtualFree(copy, 0, MEM_RELEASE);
			return NULL;
		}
		rel = destination >= next ? (int32_t)(destination - next) : (int32_t)(-(int64_t)(next - destination));
		memcpy(copy + calls[i] + 1, &rel, sizeof(rel));
	}
	/* Internal branches retain their offsets. The fall-through clears
	 * ownership without changing any native register or arithmetic flag. */
	memcpy(copy + sizeof(monthly_image), "\xFF\x25\0\0\0\0", 6);
	{ void *done = (void *)country_replay_done; memcpy(copy + sizeof(monthly_image) + 6, &done, sizeof(done)); }
	if (!FlushInstructionCache(GetCurrentProcess(), copy, sizeof(monthly_image) + 14)) {
		VirtualFree(copy, 0, MEM_RELEASE);
		return NULL;
	}
	return copy;
}

static int32_t init(const FochHostApi *host) {
	static const char pattern[] = "49 8B 87 E8 1C 00 00 45 0F B6 C4 41 0F B6 D5";
	void *site = NULL, *state_site = NULL, *active_at = NULL, *daily_a_at = NULL, *daily_b_at = NULL, *target = NULL;
	unsigned char *code;
	int32_t rel = 0, rc = FOCH_E_INVALID, rollback_rc = FOCH_OK, commit_rc = FOCH_OK;
	int found = FOCH_E_NOT_FOUND, created = FOCH_E_INVALID, enabled = FOCH_E_INVALID;
	int state_found = FOCH_E_NOT_FOUND, state_created = 0, state_enabled = 0;
	int active_found = FOCH_E_NOT_FOUND, active_created = 0, active_enabled = 0;
	int daily_a_found = FOCH_E_NOT_FOUND, daily_a_rc = FOCH_OK;
	int daily_b_found = 0, daily_b_rc = FOCH_OK;
	char proof[1280];
	capture_proof_root();
	plugin_status = (FochStatus){ sizeof(FochStatus), FOCH_STATE_INITIALIZING, 0, { NULL, 0 } };
	if (!host || host->size < offsetof(FochHostApi, hook_set_enabled) + sizeof(host->hook_set_enabled) ||
		host->abi_major != FOCH_ABI_MAJOR || !host->exe_base || !host->exe_size ||
		!host->find_pattern || !host->hook_create || !host->hook_set_enabled || !host->report) {
		plugin_status = (FochStatus){ sizeof(FochStatus), FOCH_STATE_FAILED, FOCH_E_INVALID, { "invalid host", 12 } };
		return FOCH_E_INVALID;
	}
	owned_count = owned_pending = 0;
	owned_preparing = 1;
	daily_a_install_started = daily_a_ready = daily_a_enabled = 0;
	daily_b_install_started = daily_b_ready = daily_b_enabled = 0;
	daily_b_real_site = NULL;
	country_splitting = states_done = active_running = active_enter_body = state_async = 0;
	active_pred = NULL;
	active_real = 0;
	active_body = NULL;
	country_body = NULL;
	if (probe_active_body() != 0) goto finished;
	found = host->find_pattern(host, (FochStr){ pattern, sizeof(pattern) - 1 }, &site);
	if (found != FOCH_OK || !site) { rc = found == FOCH_OK ? FOCH_E_INVALID : found; goto finished; }
	code = site;
	if (!daily_a_readable(host->exe_base, host->exe_size, code, 0x4A) ||
		!monthly_profile_is_known(host, code) || !image_code(host, code + 0x49)) {
		found = FOCH_E_INVALID;
		goto finished;
	}
	memcpy(&rel, code + 0x14, sizeof(rel));
	if (!daily_a_target(code + 0x13, 5, rel, &target) || !image_code(host, target)) goto finished;
	country_monthly = (MonthlyFn)target;
	country_site = site;
	country_after = code + 0x49;
	country_body = build_monthly_loop(host, code);
	if (!country_body) { rc = FOCH_E_MEMORY; goto finished; }
	rc = install_state(host, &state_found, &state_created, &state_enabled, &state_site);
	if (rc != FOCH_OK) goto finished;
	created = owned_create(host, site, (void *)country_detour, 10, &country_next, &country_hook);
	if (created != FOCH_OK) { rc = created; goto finished; }
	enabled = owned_enable(host, country_hook);
	if (enabled != FOCH_OK) { rc = enabled; goto finished; }
	rc = install_active(host, &active_found, &active_created, &active_enabled, &active_at);
	if (rc != FOCH_OK) goto finished;
	{
		char value[8];
		DWORD len = GetEnvironmentVariableA("FOCH_DAILY_A", value, sizeof(value));
		if (len > 0 && len < sizeof(value) && value[0] == '1') daily_a_enabled = 1;
	}
	daily_a_rc = install_daily_a(host, &daily_a_found, &daily_a_at);
	/* An unselected or unsupported daily profile does not remove the
	 * selected monthly/state/active members. A failed installation does. */
	if (daily_a_rc != FOCH_OK && daily_a_install_started) { rc = daily_a_rc; goto finished; }
	{
		char value[8];
		DWORD len = GetEnvironmentVariableA("FOCH_DAILY_B", value, sizeof(value));
		if (len > 0 && len < sizeof(value) && value[0] == '1') daily_b_enabled = 1;
	}
	daily_b_rc = install_daily_b(host, &daily_b_found, &daily_b_at);
	if (daily_b_rc != FOCH_OK && daily_b_install_started) { rc = daily_b_rc; goto finished; }
	commit_rc = owned_commit(host);
	rc = commit_rc;
finished:
	if (rc != FOCH_OK) {
		rollback_rc = owned_rollback(host);
		if (rollback_rc == FOCH_OK) {
			country_splitting = states_done = active_running = active_enter_body = state_async = 0;
			daily_a_ready = daily_b_ready = 0;
			if (country_body && !VirtualFree(country_body, 0, MEM_RELEASE)) rollback_rc = FOCH_E_MEMORY;
			else country_body = NULL;
			if (active_body && !VirtualFree(active_body, 0, MEM_RELEASE)) rollback_rc = FOCH_E_MEMORY;
			else active_body = NULL;
			active_pred = NULL;
			active_real = 0;
		}
		if (rollback_rc != FOCH_OK) {
			rc = rollback_rc;
			plugin_status = (FochStatus){ sizeof(FochStatus), FOCH_STATE_FAILED, rc, { "country month rollback failed", 29 } };
		} else plugin_status = (FochStatus){ sizeof(FochStatus), FOCH_STATE_FAILED, rc, { "country month init failed", 25 } };
	} else plugin_status = (FochStatus){ sizeof(FochStatus), FOCH_STATE_ACTIVE, 0, { "country month", 13 } };
	owned_preparing = 0;
	snprintf(proof, sizeof(proof),
		"found=%d\ncreated=%d\nenabled=%d\nsite=%p\nmonthly=%p\n"
		"state-found=%d\nstate-created=%d\nstate-enabled=%d\nstate=%p\n"
		"active-found=%d\nactive-created=%d\nactive-enabled=%d\nactive=%p\nactive-real=%d\n"
		"daily-a-found=%d\ndaily-a-rc=%d\ndaily-a=%p\ndaily-a-ready=%d\ndaily-a-alloc=%d\ndaily-a-step=%d\n"
		"daily-a-enabled=%d\ndaily-b-found=%d\ndaily-b-rc=%d\ndaily-b=%p\ndaily-b-ready=%d\n"
		"daily-b-enabled=%d\ndaily-b-real=%p\ninit-rc=%d\ncommit-rc=%d\nrollback-rc=%d\n",
		found, created, enabled, site, (void *)country_monthly,
		state_found, state_created, state_enabled, state_site,
		active_found, active_created, active_enabled, active_at, active_real,
		daily_a_found, daily_a_rc, daily_a_at, daily_a_ready, daily_a_alloc_failed, daily_a_step,
		daily_a_enabled, daily_b_found, daily_b_rc, daily_b_at, daily_b_ready,
		daily_b_enabled, daily_b_real_site, rc, commit_rc, rollback_rc);
	write_proof("countries-entry.txt", proof);
	host->report(host, &plugin_status);
	return rc;
}

static void status(FochStatus *out) {
	*out = plugin_status;
}

static const FochPluginApi api = {
	sizeof(FochPluginApi), FOCH_ABI_MAJOR, FOCH_ABI_MINOR, 0,
	init, status, NULL, NULL, NULL
};

__declspec(dllexport) const FochPluginApi *foch_plugin_get_api(uint32_t major) {
	(void)major;
	return &api;
}
