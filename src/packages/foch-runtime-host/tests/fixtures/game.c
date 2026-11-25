#include <windows.h>
#include <winver.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *exports[] = {
	"GetFileVersionInfoA", "GetFileVersionInfoByHandle", "GetFileVersionInfoExA",
	"GetFileVersionInfoExW", "GetFileVersionInfoSizeA", "GetFileVersionInfoSizeExA",
	"GetFileVersionInfoSizeExW", "GetFileVersionInfoSizeW", "GetFileVersionInfoW",
	"VerFindFileA", "VerFindFileW", "VerInstallFileA", "VerInstallFileW",
	"VerLanguageNameA", "VerLanguageNameW", "VerQueryValueA", "VerQueryValueW"
};

extern int fixture_hookable(void);
extern int fixture_short(void);
extern int fixture_late(void);
extern void fixture_unique(void);
extern void fixture_ambiguous_a(void);
extern void fixture_ambiguous_b(void);
extern int fixture_run_countries(void *world, unsigned long long index, unsigned long long end,
	unsigned long long r12_value, unsigned char dl);
extern unsigned char *fixture_country_site(void);
extern unsigned long long fixture_country_span(void);
extern unsigned fixture_run_active(void *game, unsigned char flag);
extern unsigned char *fixture_active_site(void);

int fixture_active_g0 = 100;
int fixture_active_g1 = 200;
int fixture_active_g2 = 300;

#define FIXTURE_COUNTRIES 32
#define STATES_EACH 2
#define STATE_VECTOR 0x24B0

struct fixture_state {
	volatile LONG hits;
	DWORD tid;
};

struct fixture_country {
	volatile LONG hits;
	DWORD tid;
	unsigned char dl;
	unsigned char r8;
	unsigned char gap[STATE_VECTOR - 10];
	struct fixture_state **states;
	struct fixture_state **states_end;
};

typedef char fixture_state_vector_at_24b0[
	offsetof(struct fixture_country, states) == STATE_VECTOR ? 1 : -1];
typedef char fixture_state_end_at_24b8[
	offsetof(struct fixture_country, states_end) == STATE_VECTOR + 8 ? 1 : -1];

#define ACTIVE_COUNTRIES 32

struct fixture_active_country {
	unsigned char before_tag[0x20];
	uint64_t tag;
	unsigned char before_d28[0xD28 - 0x28];
	uint32_t field_d28;
	unsigned char before_kind[0x1344 - 0xD2C];
	uint32_t field_1344;
	int32_t accept;
	DWORD pred_tid;
};

struct fixture_active_world {
	unsigned char before_countries[0x1CE8];
	void **countries;
	void **countries_end;
	unsigned char before_aux[8];
	unsigned char aux_before_size[0x14];
	uint32_t aux_size;
	void *list1_object;
	uint64_t *list1_data;
	uint32_t list1_cap;
	uint32_t list1_size;
	void **list2_begin;
	void **list2_end;
	void **list2_cap;
};

typedef char fixture_active_tag_at_20[offsetof(struct fixture_active_country, tag) == 0x20 ? 1 : -1];
typedef char fixture_active_d28_at[offsetof(struct fixture_active_country, field_d28) == 0xD28 ? 1 : -1];
typedef char fixture_active_kind_at[offsetof(struct fixture_active_country, field_1344) == 0x1344 ? 1 : -1];
typedef char fixture_active_accept_at[offsetof(struct fixture_active_country, accept) == 0x1348 ? 1 : -1];
typedef char fixture_active_pred_at[offsetof(struct fixture_active_country, pred_tid) == 0x134C ? 1 : -1];
typedef char fixture_active_countries_at[offsetof(struct fixture_active_world, countries) == 0x1CE8 ? 1 : -1];
typedef char fixture_active_aux_at[offsetof(struct fixture_active_world, aux_size) == 0x1D14 ? 1 : -1];
typedef char fixture_active_list1_at[offsetof(struct fixture_active_world, list1_object) == 0x1D18 ? 1 : -1];
typedef char fixture_active_list1_size_at[offsetof(struct fixture_active_world, list1_size) == 0x1D2C ? 1 : -1];
typedef char fixture_active_list2_at[offsetof(struct fixture_active_world, list2_begin) == 0x1D30 ? 1 : -1];
typedef char fixture_active_list2_cap_at[offsetof(struct fixture_active_world, list2_cap) == 0x1D40 ? 1 : -1];

static struct fixture_country g_countries[FIXTURE_COUNTRIES];
static struct fixture_state g_states[FIXTURE_COUNTRIES][STATES_EACH];
static struct fixture_state *g_state_ptrs[FIXTURE_COUNTRIES][STATES_EACH];
static void *g_ptrs[FIXTURE_COUNTRIES];
static unsigned char g_world[0x1d00];
static volatile LONG g_hits;
static volatile LONG g_hits_at_after;
static DWORD g_caller;

extern void fixture_state_entry(void *state);

/* ===== Bounded two-thread spread rendezvous =====
   The takeover proofs must show judgment/state work on at least two pool
   threads, but the Windows threadpool spawns extra threads lazily: one
   thread can drain every queued part before a second thread exists (all
   observed CI collapses finished in under 0.3 ms). The first distinct
   worker entering the rendezvous therefore blocks on a manual-reset event
   until a second distinct worker arrives, which forces the pool to run the
   remaining queued parts elsewhere; a 100 ms safety timeout bounds the wait
   if no second thread ever comes. Serial controls, native fallbacks and
   unhooked baselines run these callbacks on the main thread and skip the
   rendezvous entirely, so only hooked pool runs pay anything. */
static DWORD g_main_tid;
static HANDLE g_spread_event;
static volatile LONG g_spread_first;
static volatile LONG g_spread_second;
static volatile LONG g_spread_gave_up;

static void spread_reset(void) {
	InterlockedExchange(&g_spread_first, 0);
	InterlockedExchange(&g_spread_second, 0);
	InterlockedExchange(&g_spread_gave_up, 0);
	if (g_spread_event) ResetEvent(g_spread_event);
}

static void spread_enter(DWORD tid) {
	LONG first;
	if (g_spread_gave_up) return;
	first = InterlockedCompareExchange(&g_spread_first, (LONG)tid, 0);
	if (first == 0) {
		if (!g_spread_event || WaitForSingleObject(g_spread_event, 100) != WAIT_OBJECT_0)
			InterlockedExchange(&g_spread_gave_up, 1);
		return;
	}
	if (first == (LONG)tid) return;
	if (InterlockedCompareExchange(&g_spread_second, (LONG)tid, 0) == 0 || g_spread_second == (LONG)tid) {
		if (g_spread_event) SetEvent(g_spread_event);
	}
}

void fixture_state_body(void *state) {
	struct fixture_state *row = state;
	DWORD tid = GetCurrentThreadId();
	if (tid != g_main_tid) spread_enter(tid);
	InterlockedIncrement(&row->hits);
	row->tid = tid;
}

void fixture_country_monthly(void *country, unsigned dl, unsigned r8_arg) {
	struct fixture_country *row = country;
	struct fixture_state **cursor;
	InterlockedIncrement(&row->hits);
	InterlockedIncrement(&g_hits);
	row->tid = GetCurrentThreadId();
	row->dl = (unsigned char)dl;
	row->r8 = (unsigned char)r8_arg;
	for (cursor = row->states; cursor < row->states_end; ++cursor) fixture_state_entry(*cursor);
}

void fixture_country_after(void) {
	g_hits_at_after = InterlockedCompareExchange(&g_hits, 0, 0);
}

static int fixture_drive_countries(void) {
	unsigned char *site;
	unsigned long long span;
	void *begin;
	void *end;
	DWORD ids[FIXTURE_COUNTRIES * STATES_EACH];
	int distinct = 0;
	int monthly_on_caller = 0;
	int state_on_caller = 0;
	int state_hits = 0;
	int state_bad = 0;
	int same_country = 1;
	int args_ok = 1;
	int i;
	int rc = 0;
	char proof[768];
	HANDLE file;
	DWORD wrote = 0;
	memset(g_world, 0, sizeof(g_world));
	memset(g_countries, 0, sizeof(g_countries));
	memset(g_states, 0, sizeof(g_states));
	g_hits = 0;
	g_hits_at_after = 0;
	for (i = 0; i < FIXTURE_COUNTRIES; ++i) {
		int s;
		g_ptrs[i] = &g_countries[i];
		for (s = 0; s < STATES_EACH; ++s) g_state_ptrs[i][s] = &g_states[i][s];
		g_countries[i].states = g_state_ptrs[i];
		g_countries[i].states_end = g_state_ptrs[i] + STATES_EACH;
	}
	begin = g_ptrs;
	end = g_ptrs + FIXTURE_COUNTRIES;
	memcpy(g_world + 0x1ce8, &begin, sizeof(begin));
	memcpy(g_world + 0x1cf0, &end, sizeof(end));
	site = fixture_country_site();
	span = fixture_country_span();
	memset(ids, 0, sizeof(ids));
	/* The 15-byte instruction window contains the 14-byte redirect and
	   one NOP; the following SIB load and call must remain untouched. */
	if (span != 0x49 || !site || site[14] != 0x90 ||
		memcmp(site + 15, "\x48\x8B\x0C\xD8", 4) != 0 || site[0x13] != 0xE8) {
		rc = 35;
	} else {
		g_caller = GetCurrentThreadId();
		spread_reset();
		fixture_run_countries(g_world, 0, FIXTURE_COUNTRIES, 0xA5, 0x3C);
		for (i = 0; i < FIXTURE_COUNTRIES; ++i) {
			int s;
			if (g_countries[i].hits != 1) rc = 31;
			if (g_countries[i].dl != 0x3C || g_countries[i].r8 != 0xA5) args_ok = 0;
			if (g_countries[i].tid == g_caller) monthly_on_caller += 1;
			if (g_states[i][0].tid == 0 || g_states[i][0].tid != g_states[i][1].tid) same_country = 0;
			for (s = 0; s < STATES_EACH; ++s) {
				int seen = 0;
				int j;
				state_hits += g_states[i][s].hits;
				if (g_states[i][s].hits != 1) state_bad = 1;
				if (g_states[i][s].tid == 0 || g_states[i][s].tid == g_caller) state_on_caller += 1;
				for (j = 0; j < distinct; ++j) if (ids[j] == g_states[i][s].tid) seen = 1;
				if (!seen && distinct < FIXTURE_COUNTRIES * STATES_EACH) ids[distinct++] = g_states[i][s].tid;
			}
		}
		if (g_hits_at_after != FIXTURE_COUNTRIES) rc = 30;
		else if (rc == 0 && !args_ok) rc = 32;
		else if (rc == 0 && monthly_on_caller != FIXTURE_COUNTRIES) rc = 33;
		else if (rc == 0 && (state_bad || state_on_caller != 0)) rc = 36;
		else if (rc == 0 && !same_country) rc = 37;
		else if (rc == 0 && distinct < 2) rc = 34;
	}
	snprintf(proof, sizeof(proof),
		"span=%llu\npattern=%s\nredirect=%s\ncountry-count=%d\nhits-at-join=%ld\n"
		"caller-thread=%lu\nmonthly-on-caller=%d\nargs=%s\n"
		"state-count=%d\nstate-hits=%d\nstate-on-caller=%d\nstate-same=%s\n"
		"state-worker-count=%d\nstate-worker-1=%lu\nstate-worker-2=%lu\n",
		span, rc == 35 ? "bad" : "ok",
		site && site[0] == 0xFF && site[1] == 0x25 ? "ff25" : "other",
		FIXTURE_COUNTRIES, g_hits_at_after, g_caller, monthly_on_caller, args_ok ? "ok" : "bad",
		FIXTURE_COUNTRIES * STATES_EACH, state_hits, state_on_caller, same_country ? "ok" : "bad",
		distinct, distinct > 0 ? ids[0] : 0, distinct > 1 ? ids[1] : 0);
	file = CreateFileA("foch-host\\countries.txt", GENERIC_WRITE, FILE_SHARE_READ, NULL,
		CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (file != INVALID_HANDLE_VALUE) {
		WriteFile(file, proof, (DWORD)strlen(proof), &wrote, NULL);
		CloseHandle(file);
	}
	printf("%s", proof);
	return rc;
}

int fixture_active_pred_body(void *country) {
	struct fixture_active_country *row = country;
	row->pred_tid = GetCurrentThreadId();
	return row->accept;
}

static int active_list1(uint64_t tag, uint32_t field_1344, uint32_t field_d28) {
	unsigned high = (unsigned)(tag >> 48);
	int mid = (int16_t)(tag >> 32);
	if (high == 7) return 0;
	if (field_1344 != 0) return 1;
	if (mid == fixture_active_g0 || mid == fixture_active_g1 || mid == fixture_active_g2) return 1;
	return field_d28 != 0;
}

static int active_list2(uint32_t flag, uint64_t tag, uint32_t field_1344) {
	unsigned high = (unsigned)(tag >> 48);
	if (flag == 0) return 0;
	return field_1344 != 0 || high == 7;
}

static char *active_append(char *cursor, char *end, const char *text) {
	size_t room;
	size_t n;
	if (cursor >= end) return end;
	room = (size_t)(end - cursor);
	n = strlen(text);
	if (n >= room) n = room - 1;
	memcpy(cursor, text, n);
	cursor[n] = 0;
	return cursor + n;
}

static int active_bytes_ok(unsigned char *site) {
	int32_t rel = 0;
	int32_t call_rel = 0;
	uint32_t magic = 0;
	unsigned char *fn;
	if (!site) return 0;
	if (site[0x6F] != 0x3B || site[0x70] != 0x0D) return 0;
	if (site[0x77] != 0x3B || site[0x78] != 0x0D) return 0;
	if (site[0x7F] != 0x3B || site[0x80] != 0x0D) return 0;
	if (site[0x157] != 0xE8) return 0;
	if (site[0x1E8] != 0x0F || site[0x1E9] != 0x8C) return 0;
	memcpy(&rel, site + 0x1EA, sizeof(rel));
	if (site + 0x1EE + rel != site + 0x25) return 0;
	if (site[0x1EE] != 0x8B) return 0;
	memcpy(&call_rel, site + 0x158, sizeof(call_rel));
	fn = site + 0x15C + call_rel;
	memcpy(&magic, fn - 4, sizeof(magic));
	return magic == 0xC0DEF00Du;
}

/* The same country population every active-loop path drives, so the timing
   rounds judge the countries the correctness pass checks. */
static void fixture_fill_active_countries(struct fixture_active_country *countries, void **ptrs) {
	int i;
	memset(countries, 0, ACTIVE_COUNTRIES * sizeof(*countries));
	for (i = 0; i < ACTIVE_COUNTRIES; ++i) {
		unsigned high = 1;
		unsigned mid = 50;
		uint32_t field_1344 = 0;
		uint32_t field_d28 = 0;
		int accept = 1;
		switch (i % 8) {
		case 0: high = 7; break;
		case 1: field_1344 = 1; accept = 0; break;
		case 2: mid = 100; break;
		case 3: break;
		case 4: field_d28 = 1; break;
		case 5: high = 7; field_1344 = 1; break;
		case 6: mid = 200; break;
		default: mid = 300; accept = 0; break;
		}
		countries[i].tag = ((uint64_t)high << 48) | ((uint64_t)mid << 32) | (uint32_t)i;
		countries[i].field_1344 = field_1344;
		countries[i].field_d28 = field_d28;
		countries[i].accept = accept;
		ptrs[i] = &countries[i];
	}
}

/* ===== Active-judge spread rendezvous =====
   The active takeover's pool workers run only plugin code and reach the
   fixture solely through the country rows, so the two-thread rendezvous
   cannot live in a callback: it rides guard pages over the row allocation.
   Each judging worker's first row access raises a guard-page violation the
   handler below turns into spread_enter(), which blocks the first distinct
   worker until a second distinct worker judges elsewhere. The pages disarm
   (one at a time, never re-armed) as threads touch them, so the wait fires
   once per run at most. Only the hooked run arms the guard: the unhooked
   baseline judges nothing on a pool and must stay timeout-free. */
static unsigned char *g_judge_lo, *g_judge_hi;
static PVOID g_judge_veh;

static LONG WINAPI judge_guard_veh(EXCEPTION_POINTERS *ep) {
	EXCEPTION_RECORD *record = ep->ExceptionRecord;
	if (record->ExceptionCode != STATUS_GUARD_PAGE_VIOLATION || record->NumberParameters < 2)
		return EXCEPTION_CONTINUE_SEARCH;
	if ((unsigned char *)record->ExceptionInformation[1] < g_judge_lo ||
		(unsigned char *)record->ExceptionInformation[1] >= g_judge_hi)
		return EXCEPTION_CONTINUE_SEARCH;
	spread_enter(GetCurrentThreadId());
	return EXCEPTION_CONTINUE_EXECUTION;
}

static int judge_guard_arm(const void *rows, size_t bytes, const unsigned char *site) {
	unsigned char *lo = (unsigned char *)rows;
	unsigned char *hi = lo + bytes;
	DWORD old = 0;
	if (!site || site[0] != 0xFF || site[1] != 0x25) return 0; /* redirect missing: unhooked run */
	g_judge_veh = AddVectoredExceptionHandler(1, judge_guard_veh);
	if (!g_judge_veh) return 0;
	g_judge_lo = lo;
	g_judge_hi = hi;
	if (!VirtualProtect(lo, hi - lo, PAGE_READWRITE | PAGE_GUARD, &old)) {
		g_judge_lo = g_judge_hi = NULL;
		RemoveVectoredExceptionHandler(g_judge_veh);
		g_judge_veh = NULL;
		return 0;
	}
	spread_reset();
	return 1;
}

static void judge_guard_disarm(void) {
	DWORD old = 0;
	if (!g_judge_veh) return;
	VirtualProtect(g_judge_lo, g_judge_hi - g_judge_lo, PAGE_READWRITE, &old);
	g_judge_lo = g_judge_hi = NULL;
	RemoveVectoredExceptionHandler(g_judge_veh);
	g_judge_veh = NULL;
}

static int fixture_drive_active(void) {
	struct fixture_active_world world;
	struct fixture_active_country *countries;
	void *ptrs[ACTIVE_COUNTRIES];
	uint64_t tag_store[ACTIVE_COUNTRIES];
	void *ptr_store[ACTIVE_COUNTRIES];
	uint64_t expect_tags[ACTIVE_COUNTRIES];
	int expect_ptrs[ACTIVE_COUNTRIES];
	unsigned char *site;
	char tags[1024];
	char ptrs_text[256];
	char proof[2048];
	char token[32];
	DWORD caller;
	unsigned sum = 0;
	uint32_t expect_sum = 0;
	int expect_tag_n = 0;
	int expect_ptr_n = 0;
	int actual_tag_n = 0;
	int actual_ptr_n = 0;
	int pred_caller = 0;
	int pred_other = 0;
	int bytes_ok;
	int lists_ok = 0;
	int pred_ok = 0;
	int aux_ok = 0;
	int rc = 0;
	int i;
	HANDLE file;
	DWORD wrote = 0;
	char *cursor;
	memset(&world, 0, sizeof(world));
	memset(tag_store, 0, sizeof(tag_store));
	memset(ptr_store, 0, sizeof(ptr_store));
	/* Page-private so the judge guard below never covers another object. */
	countries = VirtualAlloc(NULL, ACTIVE_COUNTRIES * sizeof(*countries),
		MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
	if (!countries) return 44;
	fixture_fill_active_countries(countries, ptrs);
	for (i = 0; i < ACTIVE_COUNTRIES; ++i) {
		if (active_list1(countries[i].tag, countries[i].field_1344, countries[i].field_d28))
			expect_tags[expect_tag_n++] = countries[i].tag;
		if (active_list2(1, countries[i].tag, countries[i].field_1344)) {
			if (countries[i].accept) {
				expect_ptrs[expect_ptr_n++] = i;
				expect_sum += 0x3E8u;
			}
		}
	}
	world.countries = ptrs;
	world.countries_end = ptrs + ACTIVE_COUNTRIES;
	world.list1_data = tag_store;
	world.list1_cap = ACTIVE_COUNTRIES;
	world.list2_begin = ptr_store;
	world.list2_end = ptr_store;
	world.list2_cap = ptr_store + ACTIVE_COUNTRIES;
	site = fixture_active_site();
	bytes_ok = active_bytes_ok(site);
	caller = GetCurrentThreadId();
	if (!bytes_ok) rc = 40;
	else {
		judge_guard_arm(countries, ACTIVE_COUNTRIES * sizeof(*countries), site);
		sum = fixture_run_active(&world, 1);
		judge_guard_disarm();
	}
	tags[0] = 0;
	cursor = tags;
	actual_tag_n = (int)world.list1_size;
	for (i = 0; i < actual_tag_n && i < ACTIVE_COUNTRIES; ++i) {
		snprintf(token, sizeof(token), "%s%llu", i ? "," : "", (unsigned long long)world.list1_data[i]);
		cursor = active_append(cursor, tags + sizeof(tags), token);
	}
	ptrs_text[0] = 0;
	cursor = ptrs_text;
	if (world.list2_end >= world.list2_begin) actual_ptr_n = (int)(world.list2_end - world.list2_begin);
	for (i = 0; i < actual_ptr_n; ++i) {
		int index = -1;
		int j;
		for (j = 0; j < ACTIVE_COUNTRIES; ++j) if (world.list2_begin[i] == ptrs[j]) index = j;
		snprintf(token, sizeof(token), "%s%d", i ? "," : "", index);
		cursor = active_append(cursor, ptrs_text + sizeof(ptrs_text), token);
	}
	lists_ok = bytes_ok && sum == expect_sum && actual_tag_n == expect_tag_n && actual_ptr_n == expect_ptr_n;
	if (lists_ok) {
		for (i = 0; i < expect_tag_n; ++i) if (world.list1_data[i] != expect_tags[i]) lists_ok = 0;
		for (i = 0; i < expect_ptr_n; ++i) if (strcmp(ptrs_text, "") == 0) lists_ok = 0;
	}
	if (lists_ok) {
		char expect_ptr_text[256];
		expect_ptr_text[0] = 0;
		cursor = expect_ptr_text;
		for (i = 0; i < expect_ptr_n; ++i) {
			snprintf(token, sizeof(token), "%s%d", i ? "," : "", expect_ptrs[i]);
			cursor = active_append(cursor, expect_ptr_text + sizeof(expect_ptr_text), token);
		}
		if (strcmp(ptrs_text, expect_ptr_text) != 0) lists_ok = 0;
	}
	for (i = 0; i < ACTIVE_COUNTRIES; ++i) {
		int gated = active_list2(1, countries[i].tag, countries[i].field_1344);
		if (!gated) {
			if (countries[i].pred_tid != 0) pred_other += 1;
		} else if (countries[i].pred_tid == caller) pred_caller += 1;
		else pred_other += 1;
	}
	pred_ok = bytes_ok && pred_other == 0 && pred_caller == 12;
	aux_ok = bytes_ok && world.aux_size == (uint32_t)expect_tag_n;
	if (rc == 0 && !lists_ok) rc = 41;
	else if (rc == 0 && !pred_ok) rc = 42;
	else if (rc == 0 && !aux_ok) rc = 43;
	snprintf(proof, sizeof(proof),
		"redirect=%s\npage=%u\nbyte-6f=%02x\nbyte-157=%02x\nbyte-1e8=%02x\nbyte-1ee=%02x\n"
		"count=%d\nsum=%u\nref-sum=%u\ntag-count=%d\nptr-count=%d\naux=%u\n"
		"pred-caller=%d\npred-other=%d\ncaller=%lu\nmatch=%s\ntags=%s\nptrs=%s\nheader-counter=%u\n",
		site && site[0] == 0xFF && site[1] == 0x25 ? "ff25" : "other",
		site ? (unsigned)((uintptr_t)site % 4096u) : 0,
		site ? site[0x6F] : 0, site ? site[0x157] : 0, site ? site[0x1E8] : 0, site ? site[0x1EE] : 0,
		ACTIVE_COUNTRIES, sum, expect_sum, actual_tag_n, actual_ptr_n, world.aux_size,
		pred_caller, pred_other, caller, rc == 0 ? "ok" : "bad", tags, ptrs_text, *(uint32_t *)((unsigned char *)&world+0x7C));
	file = CreateFileA("foch-host\\active-game.txt", GENERIC_WRITE, FILE_SHARE_READ, NULL,
		CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (file != INVALID_HANDLE_VALUE) {
		WriteFile(file, proof, (DWORD)strlen(proof), &wrote, NULL);
		CloseHandle(file);
	}
	printf("%s", proof);
	VirtualFree(countries, 0, MEM_RELEASE);
	return rc;
}

#define TIMING_WARMUP 8
#define TIMING_ROUNDS 4000

static long long timing_ticks_to_ns(LONGLONG ticks) {
	LARGE_INTEGER freq;
	QueryPerformanceFrequency(&freq);
	if (freq.QuadPart <= 0) return 0;
	return (ticks / freq.QuadPart) * 1000000000LL
		+ (ticks % freq.QuadPart) * 1000000000LL / freq.QuadPart;
}

static int timing_cmp(const void *left, const void *right) {
	long long a = *(const long long *)left;
	long long b = *(const long long *)right;
	return a < b ? -1 : a > b ? 1 : 0;
}

static long long timing_median(long long *values, int n) {
	qsort(values, (size_t)n, sizeof(*values), timing_cmp);
	return values[n / 2];
}

/* Times the takeover against the original loop on the same country
   population. The unhooked process reports the plain loop's median; the
   hooked process also pulls the plugin's own spans (judgment, write-back,
   total) through the exported getter, so the ratio of the plugin's total to
   plain loop says whether this slice got faster. */
static int fixture_time_active(void) {
	struct fixture_active_world world;
	struct fixture_active_country countries[ACTIVE_COUNTRIES];
	void *ptrs[ACTIVE_COUNTRIES];
	uint64_t tag_store[ACTIVE_COUNTRIES];
	void *ptr_store[ACTIVE_COUNTRIES];
	typedef int (*SpansFn)(long long *, long long *, long long *);
	SpansFn spans_fn = NULL;
	HMODULE plugin;
	long long *loop_ns, *judge_ns, *writeback_ns, *total_ns;
	long long median_loop, median_judge = 0, median_writeback = 0, median_total = 0;
	LARGE_INTEGER t0, t1;
	char proof[512];
	HANDLE file;
	DWORD wrote = 0;
	int have_spans = 0;
	int hooked;
	int i, r;
	int rc = 0;

	memset(&world, 0, sizeof(world));
	memset(tag_store, 0, sizeof(tag_store));
	memset(ptr_store, 0, sizeof(ptr_store));
	fixture_fill_active_countries(countries, ptrs);
	world.countries = ptrs;
	world.countries_end = ptrs + ACTIVE_COUNTRIES;
	world.list1_data = tag_store;
	world.list1_cap = ACTIVE_COUNTRIES;
	world.list2_begin = ptr_store;
	world.list2_end = ptr_store;
	world.list2_cap = ptr_store + ACTIVE_COUNTRIES;

	plugin = GetModuleHandleA("eu4_country_month.dll");
	if (plugin) spans_fn = (SpansFn)GetProcAddress(plugin, "foch_active_last_spans");
	hooked = spans_fn != NULL;

	loop_ns = malloc(TIMING_ROUNDS * sizeof(*loop_ns));
	judge_ns = malloc(TIMING_ROUNDS * sizeof(*judge_ns));
	writeback_ns = malloc(TIMING_ROUNDS * sizeof(*writeback_ns));
	total_ns = malloc(TIMING_ROUNDS * sizeof(*total_ns));
	if (!loop_ns || !judge_ns || !writeback_ns || !total_ns) rc = 51;
	for (r = 0; rc == 0 && r < TIMING_WARMUP + TIMING_ROUNDS; ++r) {
		world.list1_size = 0;
		world.aux_size = 0;
		world.list2_end = world.list2_begin;
		QueryPerformanceCounter(&t0);
		fixture_run_active(&world, 1);
		QueryPerformanceCounter(&t1);
		if (r < TIMING_WARMUP) continue;
		i = r - TIMING_WARMUP;
		loop_ns[i] = timing_ticks_to_ns(t1.QuadPart - t0.QuadPart);
		if (spans_fn) {
			long long judge = 0;
			long long writeback = 0;
			long long total = 0;
			if (spans_fn(&judge, &writeback, &total)) {
				judge_ns[i] = judge;
				writeback_ns[i] = writeback;
				total_ns[i] = total;
				have_spans = 1;
			}
		}
	}
	if (rc == 0) {
		median_loop = timing_median(loop_ns, TIMING_ROUNDS);
		if (have_spans) {
			median_judge = timing_median(judge_ns, TIMING_ROUNDS);
			median_writeback = timing_median(writeback_ns, TIMING_ROUNDS);
			median_total = timing_median(total_ns, TIMING_ROUNDS);
		}
		snprintf(proof, sizeof(proof),
			"rounds=%d\nwarmup=%d\nhooked=%d\nmedian-loop-ns=%lld\n"
			"median-judge-ns=%lld\nmedian-writeback-ns=%lld\nmedian-total-ns=%lld\n",
			TIMING_ROUNDS, TIMING_WARMUP, hooked, median_loop,
			median_judge, median_writeback, median_total);
		file = CreateFileA("foch-host\\active-timing.txt", GENERIC_WRITE, FILE_SHARE_READ, NULL,
			CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
		if (file != INVALID_HANDLE_VALUE) {
			WriteFile(file, proof, (DWORD)strlen(proof), &wrote, NULL);
			CloseHandle(file);
		}
		printf("%s", proof);
	}
	free(loop_ns);
	free(judge_ns);
	free(writeback_ns);
	free(total_ns);
	return rc;
}

__declspec(dllexport) void *fixture_hookable_site(void) { return (void *)fixture_hookable; }
__declspec(dllexport) void *fixture_short_site(void) { return (void *)fixture_short; }
__declspec(dllexport) void *fixture_late_site(void) { return (void *)fixture_late; }
__declspec(dllexport) void *fixture_unique_site(void) { return (void *)fixture_unique; }
__declspec(dllexport) void *fixture_ambiguous_site(void) {
	static void *keep[2];
	keep[0] = (void *)fixture_ambiguous_a;
	keep[1] = (void *)fixture_ambiguous_b;
	return keep[0];
}

static int native_at_constructor;
static void stage(const wchar_t *name) {
	HANDLE file = CreateFileW(name, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
		NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
}

static void before_main(void) {
	HMODULE native = GetModuleHandleW(L"native.dll");
	stage(L"foch-host\\game-constructor-entered");
	g_main_tid = GetCurrentThreadId();
	g_spread_event = CreateEventA(NULL, TRUE, FALSE, NULL);
	if (native) {
		typedef int (*ReadyFn)(void);
		ReadyFn ready = (ReadyFn)GetProcAddress(native, "FixtureInitialized");
		native_at_constructor = ready && ready() == 1;
	}
}
#pragma section(".CRT$XCU", read)
__declspec(allocate(".CRT$XCU")) void (__cdecl *fixture_constructor)(void) = before_main;

static DWORD WINAPI first_version_call(void *sample) {
	DWORD ignored = 0;
	return GetFileVersionInfoSizeW((LPCWSTR)sample, &ignored) ? 0 : 1;
}

/* ===== Daily queue-drain stand-in (fixture_queue in sites.asm) =====
   The stand-in keeps the eu4.exe body's patch-site shapes at the same
   offsets; its five calls carry zero rel32 placeholders until patched
   here. The plugin installs its daily-A hook on the head in fixture
   mode, so calling fixture_queue goes through the worker path, while an
   unhooked executable copy provides the serial baseline. */

#define DAILY_A_QUEUE_BYTES 0x14C1
#define DAILY_A_CALLS 4

struct fixture_da_country {
	unsigned char pad0[0x48];
	void *war_begin;   /* +0x48 pair: non-empty means the heavy path */
	void *war_end;
	int payload;
	int heavy_marks;
};

struct fixture_da_world {
	unsigned char pad0[0x1E28];
	unsigned char pad1[8];      /* +0x1e28 */
	void **array;               /* +0x1e30 */
	unsigned char pad2[4];      /* +0x1e38 */
	long count;                 /* +0x1e3c */
	void *head;                 /* +0x1e40 */
	void *tail;                 /* +0x1e48 */
	long node_count;            /* +0x1e50 */
};

extern unsigned char fixture_queue[];


/* Allocate executable memory within rel32 reach of the anchor, so patched
   E8s in the copy can reach the module's C stubs. */
static unsigned char *exec_near(void *anchor, size_t bytes) {
	SYSTEM_INFO info;
	MEMORY_BASIC_INFORMATION region;
	unsigned char *base = anchor;
	size_t granularity;
 uintptr_t cursor;
 GetSystemInfo(&info);
 granularity = info.dwAllocationGranularity ? info.dwAllocationGranularity : 0x10000;
 cursor = ((uintptr_t)base & ~(uintptr_t)(granularity - 1));
 for (int steps = 0; steps < 4096; ++steps) {
  uintptr_t next;
  if (!VirtualQuery((void *)cursor, &region, sizeof(region))) break;
  next = (uintptr_t)region.BaseAddress + region.RegionSize;
  if (region.State == MEM_FREE) {
   uintptr_t start = ((uintptr_t)region.BaseAddress + granularity - 1) & ~(uintptr_t)(granularity - 1);
   if (start < (uintptr_t)base - 0x70000000u) start = (uintptr_t)base - 0x70000000u;
   if (start + bytes <= next) {
    void *block = VirtualAlloc((void *)start, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
    if (block) return block;
   }
  }
  if (next <= cursor) break;
  cursor = next;
 }
 return NULL;
}

extern void fixture_daily_alloc_wrap(void);
extern unsigned long long da_shim(void *fn, void *world);
extern void fixture_daily_stack_probe(void);
extern int fixture_daily_fallback_invoke(void *detour, void *context);
extern void fixture_daily_fallback_next(void);
extern void fixture_trace_invoke(void *fn, void *world, void *guard);
extern void fixture_trace_work(void *counter);
extern unsigned long long fixture_trace_flags(void);
extern unsigned char fixture_trace_arm[], fixture_trace_stop[];
extern void fixture_daily_worker_entry_probe(void);
extern unsigned int fixture_daily_worker_frame(void *worker, void *part);
extern void fixture_daily_bad_worker(void);
void *fixture_worker_return;
unsigned int fixture_worker_result;
extern void fixture_daily_free_wrap(void);
/* The plugin resolves the funnel wrappers during its entry init, which runs
   before main patches the stand-in's calls, so the addresses travel here. */
__declspec(dllexport) void *fixture_daily_alloc_wrap_addr(void) {
	return (void *)fixture_daily_alloc_wrap;
}

__declspec(dllexport) void *fixture_daily_free_wrap_addr(void) {
	return (void *)fixture_daily_free_wrap;
}

void *fixture_daily_alloc(unsigned long size) {
	return HeapAlloc(GetProcessHeap(), 0, size);
}

static volatile LONG fixture_native_free, fixture_foreign_free;
/* The native fixture allocator really frees its own blocks and records a
   provenance violation before an invalid HeapFree could terminate the child. */
void fixture_daily_free(void *ptr, unsigned long size) {
	(void)size;
	if (!HeapValidate(GetProcessHeap(), 0, ptr)) { InterlockedIncrement(&fixture_foreign_free); return; }
	InterlockedIncrement(&fixture_native_free);
	if (!HeapFree(GetProcessHeap(), 0, ptr)) InterlockedIncrement(&fixture_foreign_free);
}

/* The 0x77ac80 shape: remove array[index] from the header at +8/+0x14. */
static void fixture_daily_shift(void *header, unsigned int index) {
	struct fixture_da_world *w = (struct fixture_da_world *)((unsigned char *)header - 0x1E28);
	long count = w->count;
	if ((long)index < count && count > 0) {
		memmove(&w->array[index], &w->array[index + 1],
			(size_t)(count - (long)index - 1) * sizeof(void *));
		w->count = count - 1;
	}
}

/* The 0xdf120 shape: tail-append {record[0]} onto the list at +0x1e40. */
static void fixture_daily_publish(void *listheader, void *record) {
	fprintf(stderr, "PUB hdr=%p world=%p cnt=%ld nodes=%ld\n", listheader,
		(void *)((char *)listheader - 0x1E40),
		((struct fixture_da_world *)((char *)listheader - 0x1E40))->count,
		((struct fixture_da_world *)((char *)listheader - 0x1E40))->node_count); fflush(stderr);
	struct fixture_da_world *w = (struct fixture_da_world *)((unsigned char *)listheader - 0x1E40);
	void **node = ((void *(*)(size_t))(void *)fixture_daily_alloc_wrap)(0x20);
	if (!node) return;
	node[0] = *(void **)record;
	node[1] = w->tail;
	node[2] = NULL;
	((unsigned char *)node)[0x18] = 0;
	if (w->node_count) ((void **)w->tail)[2] = node;
	else w->head = node;
	w->tail = node;
	w->node_count += 1;
}

extern void fixture_daily_home_work(void *country);
static int fixture_daily_home;
void fixture_heavy_work(void *country) {
	struct fixture_da_country *c = country;
	void *allocation = ((void *(*)(size_t))(void *)fixture_daily_alloc_wrap)(32);
	if (!allocation) return;
	memset(allocation, 0x5A, 32);
	((void (*)(void *, size_t))(void *)fixture_daily_free_wrap)(allocation, 32);
	c->payload += 1;
	c->heavy_marks += 1;
}

static void patch_da_calls(unsigned char *base) {
	static const struct { unsigned int at; void *fn; } fixes[] = {
		{0x104, NULL}, {0x15A, NULL}, {0x13DE, NULL}, {0x13F9, NULL},
	};
	void *fns[DAILY_A_CALLS];
	unsigned long old_protect = 0;
	int i;
	fns[0] = (void *)fixture_daily_alloc_wrap;
	fns[1] = fixture_daily_home ? (void *)fixture_daily_home_work : (void *)fixture_heavy_work;
	fns[2] = (void *)fixture_daily_shift;
	fns[3] = (void *)fixture_daily_publish;
	fprintf(stderr, "fns: alloc=%p heavy=%p shift=%p publish=%p\n", fns[0], fns[1], fns[2], fns[3]); fflush(stderr);
	VirtualProtect(base, DAILY_A_QUEUE_BYTES, PAGE_EXECUTE_READWRITE, &old_protect);
	for (i = 0; i < DAILY_A_CALLS; ++i) {
		ptrdiff_t rel = (ptrdiff_t)fns[i] - (ptrdiff_t)(base + fixes[i].at + 5);
		memcpy(base + fixes[i].at + 1, &rel, 4);
	}
	FlushInstructionCache(GetCurrentProcess(), base, DAILY_A_QUEUE_BYTES);
	VirtualProtect(base, DAILY_A_QUEUE_BYTES, old_protect, &old_protect);
}

#define DA_COUNTRIES 37

static void da_fill(struct fixture_da_world *w, struct fixture_da_country *countries,
		void **slots, int salt) {
	int i;
	memset(w, 0, sizeof(*w));
	memset(countries, 0, sizeof(*countries) * DA_COUNTRIES);
	memset(slots, 0, sizeof(*slots) * DA_COUNTRIES);
	for (i = 0; i < DA_COUNTRIES; ++i) {
		int heavy = ((i * 7 + salt) % 3) == 0;
		countries[i].payload = i;
		if (heavy) {
			countries[i].war_begin = &countries[i].payload;
			countries[i].war_end = ((char *)&countries[i].payload) + 16;
		}
		slots[i] = &countries[i];
	}
	w->array = slots;
	w->count = DA_COUNTRIES;
}

static int da_country_id(void *country, struct fixture_da_country *countries) {
	int i;
	for (i = 0; i < DA_COUNTRIES; ++i) if (country == &countries[i]) return i;
	return -1;
}

static int da_list_matches(struct fixture_da_world *a, struct fixture_da_world *b,
		struct fixture_da_country *ac, struct fixture_da_country *bc) {
	void *na = a->head;
	void *nb = b->head;
	void *pa = NULL, *pb = NULL;
	long i;
	if (a->node_count != b->node_count || a->node_count < 0 || a->node_count > DA_COUNTRIES) return 0;
	for (i = 0; i < a->node_count; ++i) {
		int id;
		if (!na || !nb) return 0;
		id = da_country_id(((void **)na)[0], ac);
		if (id < 0 || id != da_country_id(((void **)nb)[0], bc)) return 0;
		if (((void **)na)[1] != pa || ((void **)nb)[1] != pb) return 0;
		pa = na;
		pb = nb;
		na = ((void **)na)[2];
		nb = ((void **)nb)[2];
	}
	return !na && !nb && a->tail == pa && b->tail == pb;
}

static void *baseline_var;
static int tf_on, tf_armed, tf_done, tf_error;
static HANDLE tf_out = INVALID_HANDLE_VALUE;
static PVOID tf_registration;
static unsigned char *tf_guard;
static DWORD tf_owner;
static unsigned long tf_steps;
static volatile LONG tf_callbacks;

static LONG WINAPI tf_veh(EXCEPTION_POINTERS *ep) {
	EXCEPTION_RECORD *record = ep->ExceptionRecord;
	CONTEXT *context = ep->ContextRecord;
	InterlockedIncrement(&tf_callbacks);
	if (GetCurrentThreadId() != tf_owner) return EXCEPTION_CONTINUE_SEARCH;
	if (record->ExceptionCode == STATUS_GUARD_PAGE_VIOLATION && tf_armed && !tf_on &&
		record->NumberParameters >= 2 && record->ExceptionInformation[0] == 0 &&
		record->ExceptionInformation[1] == (ULONG_PTR)tf_guard &&
		record->ExceptionAddress == (void *)fixture_trace_arm &&
		context->Rip == (DWORD64)(void *)fixture_trace_arm && !(context->EFlags & 0x100)) {
		tf_armed = 0;
		tf_on = 1;
		context->EFlags |= 0x100;
		return EXCEPTION_CONTINUE_EXECUTION;
	}
	if (record->ExceptionCode == EXCEPTION_SINGLE_STEP && tf_on &&
		record->ExceptionAddress == (void *)context->Rip &&
		!(context->Dr6 & 0xA00F)) {
		DWORD wrote = 0;
		char line[256];
		int len;
		// Windows can supply TF clear and DR6=0 for our trace traps. The
		// owner thread, exact guard trigger and live session establish ownership.
		if (context->Rip == (DWORD64)(void *)fixture_trace_stop) {
			context->EFlags &= ~0x100u;
			tf_on = 0;
			tf_done = 1;
			return EXCEPTION_CONTINUE_EXECUTION;
		}
		len = snprintf(line, sizeof(line),
			"%llx rax=%llx rcx=%llx rdx=%llx r12=%llx r15=%llx flags=%lx dr6=%llx%s\n",
			(unsigned long long)context->Rip, (unsigned long long)context->Rax,
			(unsigned long long)context->Rcx, (unsigned long long)context->Rdx,
			(unsigned long long)context->R12, (unsigned long long)context->R15,
			(unsigned long)context->EFlags, (unsigned long long)context->Dr6,
			(context->Rip >= (DWORD64)baseline_var && context->Rip - (DWORD64)baseline_var < 0x1500) ? " B" : "");
		if (len < 0 || len >= (int)sizeof(line) ||
			!WriteFile(tf_out, line, (DWORD)len, &wrote, NULL) || wrote != (DWORD)len) {
			tf_error = 1;
			context->EFlags &= ~0x100u;
			tf_on = 0;
			tf_done = 1;
			return EXCEPTION_CONTINUE_EXECUTION;
		}
		++tf_steps;
		context->EFlags |= 0x100;
		return EXCEPTION_CONTINUE_EXECUTION;
	}
	return EXCEPTION_CONTINUE_SEARCH;
}

static int tf_begin(const char *path, void *base) {
	if (tf_registration || tf_on || (fixture_trace_flags() & 0x100) || IsDebuggerPresent()) return 0;
	tf_on = tf_done = tf_error = 0;
	tf_steps = 0;
	baseline_var = base;
	tf_out = CreateFileA(path, GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (tf_out == INVALID_HANDLE_VALUE) return 0;
	tf_guard = VirtualAlloc(NULL, 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE | PAGE_GUARD);
	if (!tf_guard) {
		if (!CloseHandle(tf_out)) tf_error = 1;
		tf_out = INVALID_HANDLE_VALUE;
		return 0;
	}
	tf_owner = GetCurrentThreadId();
	tf_armed = 1;
	tf_registration = AddVectoredExceptionHandler(1, tf_veh);
	if (!tf_registration) {
		tf_armed = 0;
		tf_owner = 0;
		if (!VirtualFree(tf_guard, 0, MEM_RELEASE)) tf_error = 1;
		if (!CloseHandle(tf_out)) tf_error = 1;
		tf_guard = NULL;
		tf_out = INVALID_HANDLE_VALUE;
		return 0;
	}
	return 1;
}

static int tf_end(void) {
	int valid = tf_done && !tf_on && !tf_error && tf_steps > 0 &&
		!(fixture_trace_flags() & 0x100) && GetCurrentThreadId() == tf_owner;
	// Never release resources while TF is still owned by the handler.
	if (tf_on || !tf_registration) return 0;
	if (!RemoveVectoredExceptionHandler(tf_registration)) return 0;
	tf_registration = NULL;
	tf_armed = 0;
	tf_owner = 0;
	if (!CloseHandle(tf_out)) valid = 0;
	tf_out = INVALID_HANDLE_VALUE;
	if (!VirtualFree(tf_guard, 0, MEM_RELEASE)) valid = 0;
	tf_guard = NULL;
	return valid;
}
static LONG WINAPI da_veh(EXCEPTION_POINTERS *ep) {
	if (ep->ExceptionRecord->ExceptionCode == 0xC0000005 || ep->ExceptionRecord->ExceptionCode == 0xC000001D) {
		HMODULE m0 = NULL;
		char nm0[160] = "?";
		if (GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (LPCWSTR)(void *)ep->ContextRecord->Rip, &m0) && m0)
			GetModuleFileNameA(m0, nm0, sizeof(nm0));
		fprintf(stderr, "EXC code=%lx rip=%p %s+%lx rax=%p rcx=%p rdx=%p r12=%p rbp=%p rsp=%p\n",
			ep->ExceptionRecord->ExceptionCode, (void *)ep->ContextRecord->Rip, nm0,
			m0 ? (unsigned long)((unsigned char *)ep->ContextRecord->Rip - (unsigned char *)m0) : 0UL,
			(void *)ep->ContextRecord->Rax, (void *)ep->ContextRecord->Rcx, (void *)ep->ContextRecord->Rdx,
			(void *)ep->ContextRecord->R12, (void *)ep->ContextRecord->Rbp, (void *)ep->ContextRecord->Rsp);
		fflush(stderr);
		fprintf(stderr, "da-crash rip=%p code=%lx addr=%p\n",
			(void *)ep->ContextRecord->Rip,
			ep->ExceptionRecord->ExceptionCode,
			(void *)ep->ExceptionRecord->ExceptionInformation[1]);
		{
			HMODULE m = NULL;
			char name[160] = "?";
			if (GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
					(LPCWSTR)(void *)ep->ContextRecord->Rip, &m) && m) {
				GetModuleFileNameA(m, name, sizeof(name));
			}
			fprintf(stderr, "module=%s off=%lx\n", name,
				m ? (unsigned long)((unsigned char *)ep->ContextRecord->Rip - (unsigned char *)m) : 0UL);
		}
	}
			{
			void *frames[24];
			USHORT nfr = CaptureStackBackTrace(0, 24, frames, NULL);
			USHORT fi;
			for (fi = 0; fi < nfr; ++fi) {
				HMODULE m = NULL;
				char nm[160] = "?";
				if (GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, (LPCWSTR)frames[fi], &m) && m) {
					GetModuleFileNameA(m, nm, sizeof(nm));
				}
				fprintf(stderr, "frame %u %s+%lx\n", fi, nm,
					m ? (unsigned long)((unsigned char *)frames[fi] - (unsigned char *)m) : 0UL);
			}
			fflush(stderr);
		}
		{
			unsigned char *rp = (unsigned char *)ep->ContextRecord->Rip;
			__try { fprintf(stderr, "bytes@rip: %02x %02x %02x %02x %02x %02x %02x %02x\n",
				rp[0],rp[1],rp[2],rp[3],rp[4],rp[5],rp[6],rp[7]); }
			__except (1) { fprintf(stderr, "bytes@rip: unreadable\n"); }
			fflush(stderr);
		}
return EXCEPTION_CONTINUE_SEARCH;
}

static void da_abi_proof(const char *name, const char *text) {
	char path[128];
	FILE *file;
	snprintf(path, sizeof(path), "foch-host/%s", name);
	file = fopen(path, "wb");
	if (file) {
		fputs(text, file);
		fclose(file);
	}
	fputs(text, stdout);
}

static DWORD WINAPI fixture_trace_foreign(void *guard) {
	DWORD seen = 0;
	__try { RaiseException(EXCEPTION_SINGLE_STEP, 0, 0, NULL); }
	__except (GetExceptionCode() == EXCEPTION_SINGLE_STEP ? EXCEPTION_EXECUTE_HANDLER : EXCEPTION_CONTINUE_SEARCH) {
		seen |= 1;
	}
	__try { (void)*(volatile unsigned char *)guard; }
	__except (GetExceptionCode() == STATUS_GUARD_PAGE_VIOLATION ? EXCEPTION_EXECUTE_HANDLER : EXCEPTION_CONTINUE_SEARCH) {
		seen |= 2;
	}
	return seen;
}

static int fixture_trace_lifecycle(void) {
	unsigned long long counter = 0;
	unsigned int cycle;
	unsigned long total_steps = 0, removed = 0;
	unsigned long foreign_owner = 0, foreign_worker = 0, callbacks_after_stop = 0;
	char text[256];
	for (cycle = 0; cycle < 2; ++cycle) {
		void *foreign;
		HANDLE worker;
		DWORD old, worker_result;
		LONG callbacks;
		if (!tf_begin("foch-host\\trace-lifecycle-steps.txt", (void *)fixture_trace_work)) return 67;
		foreign = VirtualAlloc(NULL, 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE | PAGE_GUARD);
		if (!foreign || fixture_trace_foreign(foreign) != 3) return 69;
		++foreign_owner;
		if (!VirtualProtect(foreign, 0x1000, PAGE_READWRITE | PAGE_GUARD, &old)) return 70;
		worker = CreateThread(NULL, 0, fixture_trace_foreign, foreign, 0, NULL);
		if (!worker || WaitForSingleObject(worker, 5000) != WAIT_OBJECT_0) return 71;
		if (!GetExitCodeThread(worker, &worker_result)) return 72;
		if (!CloseHandle(worker) || worker_result != 3 || tf_on || !tf_armed || tf_steps) return 73;
		++foreign_worker;
		fixture_trace_invoke((void *)fixture_trace_work, &counter, tf_guard);
		if (!tf_end()) return 68;
		++removed;
		total_steps += tf_steps;
		callbacks = tf_callbacks;
		if (!VirtualProtect(foreign, 0x1000, PAGE_READWRITE | PAGE_GUARD, &old)) return 74;
		if (fixture_trace_foreign(foreign) != 3) return 75;
		callbacks_after_stop += (unsigned long)(tf_callbacks - callbacks);
		++foreign_owner;
		if (!VirtualFree(foreign, 0, MEM_RELEASE)) return 76;
	}
	snprintf(text, sizeof(text), "counter=%llu\nsteps=%lu\nremoved=%lu\ntrap-flag=%llu\nforeign-owner=%lu\nforeign-worker=%lu\ncallbacks-after-stop=%lu\n",
		counter, total_steps, removed, fixture_trace_flags() & 0x100,
		foreign_owner, foreign_worker, callbacks_after_stop);
	da_abi_proof("trace-lifecycle.txt", text);
	return 0;
}

static int fixture_trace_unowned(void) {
	EXCEPTION_RECORD record = {0};
	CONTEXT context = {0};
	EXCEPTION_POINTERS ep = {&record, &context};
	LONG guard_result, step_result;
	DWORD guard_flags;
	char text[192];
	record.ExceptionCode = STATUS_GUARD_PAGE_VIOLATION;
	record.ExceptionAddress = (void *)fixture_trace_work;
	record.NumberParameters = 2;
	record.ExceptionInformation[1] = (ULONG_PTR)fixture_trace_work;
	context.Rip = (DWORD64)(void *)fixture_trace_work;
	context.EFlags = 0x202;
	guard_result = tf_veh(&ep);
	guard_flags = context.EFlags;
	tf_on = 0;
	record.ExceptionCode = EXCEPTION_SINGLE_STEP;
	context.Rip = (DWORD64)(void *)fixture_trace_stop;
	step_result = tf_veh(&ep);
	snprintf(text, sizeof(text), "guard-disposition=%ld\nguard-flags=%lu\nstep-disposition=%ld\n",
		guard_result, guard_flags, step_result);
	da_abi_proof("trace-unowned.txt", text);
	return 0;
}

static int fixture_daily_worker_abi(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef void (*WorkerFn)(void *);
	WorkerFn worker;
	struct fixture_daily_worker_part {
		void **snapshot;
		unsigned char *shadow;
		void *copy;
		void *game;
		uint64_t from, to;
	} part;
	void *slots[9] = {0};
	unsigned int failures = 0, case_index;
	char text[128];
	if (!plugin) return 77;
	worker = (WorkerFn)GetProcAddress(plugin, "daily_a_worker");
	fixture_worker_return = (void *)GetProcAddress(plugin, "daily_a_worker_ret");
	if (!worker || !fixture_worker_return || offsetof(struct fixture_daily_worker_part, to) != 0x28) return 78;
	memset(&part, 0, sizeof(part));
	part.shadow = VirtualAlloc(NULL, 0x800, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
	if (!part.shadow) return 79;
	part.snapshot = slots;
	part.copy = (void *)fixture_daily_worker_entry_probe;
	part.game = &part;
	slots[0] = slots[8] = (void *)0x2468;
	memcpy(part.shadow + 0x300 + 0x1C8, &part.snapshot, sizeof(part.snapshot));
	for (case_index = 0; case_index < 2; ++case_index) {
		part.from = case_index ? 5 : 0;
		part.to = case_index ? 9 : 1;
		fixture_worker_result = 99;
		worker(&part);
		if (fixture_worker_result) failures |= fixture_worker_result << (case_index * 8);
	}
	if (!VirtualFree(part.shadow, 0, MEM_RELEASE)) return 80;
	snprintf(text, sizeof(text), "cases=2\nentry-errors=%u\n", failures);
	da_abi_proof("daily-worker-abi.txt", text);
	return 0;
}

static int fixture_daily_fallback_abi(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	void *detour;
	volatile LONG *running;
	int *ready;
	void *const **next;
	void *const *saved_next;
	LONG saved_running, unready_owner, nested_owner;
	int saved_ready, unready, nested;
	void *tail = (void *)fixture_daily_fallback_next;
	void *slots[] = {(void *)0x1357, (void *)0x2468};
	struct { void *unused; void **array; } context = {NULL, slots};
	char text[192];
	if (!plugin) return 65;
	detour = (void *)GetProcAddress(plugin, "daily_a_detour");
	running = (volatile LONG *)GetProcAddress(plugin, "daily_a_running");
	ready = (int *)GetProcAddress(plugin, "daily_a_ready");
	next = (void *const **)GetProcAddress(plugin, "daily_a_next");
	if (!detour || !running || !ready || !next) return 66;
	saved_next = *next;
	saved_running = *running;
	saved_ready = *ready;
	*next = &tail;
	*running = 0;
	*ready = 0;
	unready = fixture_daily_fallback_invoke(detour, &context);
	unready_owner = *running;
	*running = 1;
	*ready = 1;
	nested = fixture_daily_fallback_invoke(detour, &context);
	nested_owner = *running;
	*next = saved_next;
	*running = saved_running;
	*ready = saved_ready;
	snprintf(text, sizeof(text),
		"unready=%d\nnested=%d\nunready-owner=%ld\nnested-owner=%ld\n",
		unready, nested, unready_owner, nested_owner);
	da_abi_proof("daily-a-fallback-abi.txt", text);
	return 0;
}

static int fixture_daily_branch(unsigned char *copy, size_t at, size_t length, size_t target) {
	int32_t rel;
	if ((length == 5 && copy[at] != (at == 0x137E ? 0xE8 : 0xE9)) ||
		(length == 6 && (copy[at] != 0x0F || copy[at + 1] != (at == 0x14CF ? 0x8C : 0x88)))) return 0;
	memcpy(&rel, copy + at + length - 4, sizeof(rel));
	return copy + at + length + rel == copy + target;
}

static int fixture_daily_builder(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef unsigned char *(*BuildFn)(void);
	typedef int (*TargetsFn)(void *, void *);
	BuildFn build = plugin ? (BuildFn)GetProcAddress(plugin, "fixture_daily_build") : NULL;
	TargetsFn targets = plugin ? (TargetsFn)GetProcAddress(plugin, "fixture_daily_replay_targets") : NULL;
	static const unsigned char head[15] = {
		0x48,0x8B,0x50,0x08,0x4C,0x8B,0x3C,0xCA,0x4C,0x89,0xBD,0xA8,0,0,0};
	unsigned char *copy;
	void *tail;
	unsigned int errors = 0;
	char text[256];
	if (!build || !targets) return 80;
	patch_da_calls(fixture_queue);
	copy = build();
	if (!copy) return 81;
	if (!fixture_daily_branch(copy, 0x97, 5, 0x1448)) errors |= 1;
	if (!fixture_daily_branch(copy, 0x137E, 5, 0x1478)) errors |= 2;
	if (!fixture_daily_branch(copy, 0x1399, 5, 0x1498)) errors |= 4;
	if (!fixture_daily_branch(copy, 0x1433, 6, 0x14B8)) errors |= 8;
	if (!fixture_daily_branch(copy, 0x1440, 5, 0x14C8)) errors |= 16;
	if (!fixture_daily_branch(copy, 0x14CF, 6, 0x14B8)) errors |= 32;
	if (!fixture_daily_branch(copy, 0x14D5, 5, 0)) errors |= 64;
	memcpy(&tail, copy + 0x1448 + 32, sizeof(tail));
	snprintf(text, sizeof(text), "native-head=%d\nhead-bytes=%02x%02x%02x%02x\nbranch-errors=%u\ntail-decrement=%lld\nreplay-targets=%d\n",
		memcmp(copy, head, sizeof(head)) == 0, copy[0], copy[1], copy[2], copy[3], errors,
		(long long)((unsigned char *)tail - copy), targets(fixture_daily_shift, fixture_daily_publish));
	da_abi_proof("daily-builder.txt", text);
	if (!VirtualFree(copy, 0, MEM_RELEASE)) return 82;
	return 0;
}

static int fixture_daily_relocation(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef unsigned char *(*BuildFn)(void *);
	BuildFn build = plugin ? (BuildFn)GetProcAddress(plugin, "fixture_daily_build_at") : NULL;
	unsigned char *image, *site, *copy, *bad;
	int32_t rel;
	int target_ok, refused;
	unsigned int i;
	const unsigned int calls[] = {0xA4,0xFA,0x137E,0x1399};
	char text[128];
	if (!build) return 87;
	image = VirtualAlloc((void *)0x10000000, 0x40000, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
	if (!image) return 88;
	site = image + 0x10000;
	memcpy(site, fixture_queue + 0x60, 0x1441);
	for (i = 0; i < 4; ++i) {
		rel = (int32_t)((image + 0x10E7) - (site + calls[i] + 5));
		memcpy(site + calls[i] + 1, &rel, 4);
	}
	site[0x200] = 0x48; site[0x201] = 0x8D; site[0x202] = 0x05;
	rel = (int32_t)((image + 0x20EF) - (site + 0x207));
	memcpy(site + 0x203, &rel, 4);
	memcpy(site + 0x210,"\x48\xB8\xE8\xE9\x0F\x85\xE8\xE9\x00\x00",10);
	copy = build(site);
	if (!copy) return 89;
	memcpy(&rel, copy + 0xFB, 4);
	target_ok = copy == (void *)0x10040000 && copy[0xFA] == 0xE8 &&
		copy + 0xFF + rel == image + 0x10E7;
	memcpy(&rel, copy + 0x203,4);
	target_ok = target_ok && copy[0x200] == 0x48 && copy[0x201] == 0x8D && copy[0x202] == 0x05 &&
		copy + 0x207 + rel == image + 0x20EF && !memcmp(copy + 0x210,site + 0x210,10) &&
		!memcmp(copy,"\x48\x8B\x50\x08\x4C\x8B\x3C\xCA\x4C\x89\xBD\xA8\x00\x00\x00",15) &&
		fixture_daily_branch(copy,0x97,5,0x1448) && fixture_daily_branch(copy,0x137E,5,0x1478) &&
		fixture_daily_branch(copy,0x1399,5,0x1498) && fixture_daily_branch(copy,0x1433,6,0x14B8) &&
		fixture_daily_branch(copy,0x1440,5,0x14C8) && fixture_daily_branch(copy,0x14CF,6,0x14B8) &&
		fixture_daily_branch(copy,0x14D5,5,0);
	if (!VirtualFree(copy, 0, MEM_RELEASE)) return 90;
	site[0x300] = 0xC5; site[0x301] = 0xF8;
	bad = build(site);
	refused = bad == NULL;
	if (bad && !VirtualFree(bad, 0, MEM_RELEASE)) return 91;
	snprintf(text, sizeof(text), "call-target=%d\nunknown-refused=%d\n", target_ok, refused);
	da_abi_proof("daily-relocation.txt", text);
	if (!VirtualFree(image, 0, MEM_RELEASE)) return 92;
	return 0;
}

static int fixture_daily_arena(unsigned int mode) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef unsigned int (*RunFn)(unsigned int);
	RunFn run = plugin ? (RunFn)GetProcAddress(plugin, "fixture_daily_arena") : NULL;
	unsigned int result;
	char text[128];
	if (!run) return 94;
	result = run(mode);
	snprintf(text, sizeof(text), "result=%u\nnative-free=%u\nforeign-free=%u\n",
		result, fixture_native_free, fixture_foreign_free);
	da_abi_proof("daily-arena.txt", text);
	return 0;
}

static int fixture_daily_rounds(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef unsigned int (*CountFn)(void);
	CountFn owners = plugin ? (CountFn)GetProcAddress(plugin, "fixture_daily_owner_count") : NULL;
	struct fixture_da_world world;
	struct fixture_da_country countries[DA_COUNTRIES];
	void *slots[DA_COUNTRIES];
	unsigned int round, errors = 0, after_parallel = 0;
	char text[160];
	if (!owners) return 96;
	patch_da_calls(fixture_queue);
	for (round = 0; round < 5; ++round) {
		void *node;
		int i;
		da_fill(&world, countries, slots, 0);
		da_shim(fixture_queue, &world);
		if (world.count || world.node_count != DA_COUNTRIES) ++errors;
		node = world.head;
		for (i = DA_COUNTRIES-1; i >= 0; --i) {
			if (!node || ((void **)node)[0] != &countries[i] || countries[i].payload != i + (i%3 == 0)) { ++errors; break; }
			node = ((void **)node)[2];
		}
		if (node) ++errors;
		if (round == 1) after_parallel = owners();
	}
	snprintf(text, sizeof(text), "errors=%u\nforeign-free=%u\nowners-after-parallel=%u\nowners-final=%u\n",
		errors, fixture_foreign_free, after_parallel, owners());
	da_abi_proof("daily-rounds.txt", text);
	return 0;
}

static int fixture_daily_range(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef int (*RangeFn)(void *, size_t, void *);
	RangeFn run = plugin ? (RangeFn)GetProcAddress(plugin, "fixture_daily_range") : NULL;
	SYSTEM_INFO info;
	MEMORY_BASIC_INFORMATION region;
	unsigned char *memory, *site;
	DWORD old;
	unsigned int refused = 0, noaccess_preserved, guard_preserved;
	char text[128];
	static const unsigned char prefix[] = {0x48,0x8B,0x50,0x08,0x4C,0x8B,0x3C,0xCA,0x4C,0x89,0xBD,0xA8,0,0,0,0x4C,0x89,0xBD,0xF8,0,0,0};
	if (!run) return 98;
	GetSystemInfo(&info);
	memory = VirtualAlloc(NULL, 3 * info.dwPageSize, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE);
	if (!memory) return 99;
	site = memory + info.dwPageSize - sizeof(prefix);
	memcpy(site, prefix, sizeof(prefix));
	if (!VirtualProtect(memory + info.dwPageSize, info.dwPageSize, PAGE_NOACCESS, &old)) return 100;
	if (run(memory, info.dwPageSize, site) != 0) ++refused;
	if (run(memory, 3 * info.dwPageSize, site) != 0) ++refused;
	if (run(memory, SIZE_MAX, site) != 0) ++refused;
	if (!VirtualQuery(memory + info.dwPageSize, &region, sizeof(region))) return 101;
	noaccess_preserved = region.Protect == PAGE_NOACCESS;
	if (!VirtualProtect(memory + info.dwPageSize, info.dwPageSize, PAGE_EXECUTE_READ | PAGE_GUARD, &old)) return 101;
	if (run(memory, 3 * info.dwPageSize, site) != 0) ++refused;
	if (!VirtualQuery(memory + info.dwPageSize, &region, sizeof(region))) return 101;
	guard_preserved = region.Protect == (PAGE_EXECUTE_READ | PAGE_GUARD);
	snprintf(text, sizeof(text), "refused=%u\nnoaccess-preserved=%u\nguard-preserved=%u\n", refused, noaccess_preserved, guard_preserved);
	da_abi_proof("daily-range.txt", text);
	if (!VirtualFree(memory, 0, MEM_RELEASE)) return 102;
	return 0;
}

static int fixture_daily_failure(const char *resource) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef void (*FaultFn)(const char *, unsigned int);
	typedef unsigned int (*HitsFn)(void);
	FaultFn fault = plugin ? (FaultFn)GetProcAddress(plugin, "fixture_daily_fault") : NULL;
	HitsFn hits = plugin ? (HitsFn)GetProcAddress(plugin, "fixture_daily_fault_hits") : NULL;
	struct fixture_da_world world;
	struct fixture_da_country countries[DA_COUNTRIES];
	void *slots[DA_COUNTRIES], *node;
	unsigned int errors = 0, fallback = 0, resumed = 0, seen, nth = 1;
	int late = strncmp(resource,"late-",5) == 0;
	volatile LONG *running = plugin ? (volatile LONG *)GetProcAddress(plugin,"daily_a_running") : NULL;
	int *ready = plugin ? (int *)GetProcAddress(plugin,"daily_a_ready") : NULL;
	int i;
	char text[192];
	FILE *proof;
	if (!fault || !hits || !running || !ready) return 104;
	patch_da_calls(fixture_queue);
	if (late) {
		da_fill(&world, countries, slots, 0);
		da_shim(fixture_queue, &world);
		resource += 5;
		if (!strcmp(resource,"ops") || !strcmp(resource,"snapshot") || !strcmp(resource,"shadow")) nth = 2;
		if (!strcmp(resource,"publish-node")) nth = 7;
	}
	da_fill(&world, countries, slots, 0);
	fault(resource, nth);
	da_shim(fixture_queue, &world);
	if (world.count || world.node_count != DA_COUNTRIES) ++errors;
	node = world.head;
	for (i = DA_COUNTRIES-1; i >= 0; --i) {
		if (!node || ((void **)node)[0] != &countries[i]) { ++errors; break; }
		node = ((void **)node)[2];
	}
	if (node) ++errors;
	for (i = 0; i < DA_COUNTRIES; ++i)
		if (countries[i].payload != i + (i%3 == 0) || countries[i].heavy_marks != (i%3 == 0)) ++errors;
	proof = fopen("foch-host/daily-a-dispatch.txt", "r");
	if (proof) { if (fgets(text, sizeof(text), proof) && strstr(text,"preflight-fallback")) fallback = 1; fclose(proof); }
	seen = hits();
	if (*running || !*ready) ++errors;
	/* The failure must release just this invocation and keep later takeover
	   usable. Verify another exact day after the one-shot fault is consumed. */
	da_fill(&world, countries, slots, 0);
	da_shim(fixture_queue, &world);
	if (!*running && *ready && !world.count && world.node_count == DA_COUNTRIES) {
		long calls = 0;
		proof = fopen("foch-host/daily-a-real.txt", "r");
		if (proof) { while (fgets(text,sizeof(text),proof)) sscanf(text,"calls=%ld",&calls); fclose(proof); }
		resumed = calls == (late ? 2 : 1);
	}
	for (i = 0; i < DA_COUNTRIES; ++i)
		if (countries[i].payload != i + (i%3 == 0) || countries[i].heavy_marks != (i%3 == 0)) ++errors;
	snprintf(text, sizeof(text), "errors=%u\nfallback=%u\nhits=%u\nresumed=%u\nforeign-free=%ld\n", errors, fallback, seen, resumed, fixture_foreign_free);
	da_abi_proof("daily-failure.txt", text);
	return 0;
}

static int fixture_daily_large_ops(void) {
	enum { count = 97 };
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef void (*FaultFn)(const char *, unsigned int);
	typedef unsigned int (*HitsFn)(void);
	FaultFn fault = plugin ? (FaultFn)GetProcAddress(plugin,"fixture_daily_fault") : NULL;
	HitsFn hits = plugin ? (HitsFn)GetProcAddress(plugin,"fixture_daily_fault_hits") : NULL;
	struct fixture_da_world world = {0};
	struct fixture_da_country *countries = HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,count*sizeof(*countries));
	void **slots = HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,count*sizeof(*slots));
	void *node;
	unsigned int errors = 0;
	int i;
	char text[128];
	if (!fault || !hits || !countries || !slots) return 106;
	for (i=0;i<count;++i) {
		countries[i].payload = i;
		countries[i].war_begin = &countries[i].payload;
		countries[i].war_end = (char *)&countries[i].payload + 16;
		slots[i] = &countries[i];
	}
	world.array=slots; world.count=count;
	patch_da_calls(fixture_queue);
	fault("ops",2);
	da_shim(fixture_queue,&world);
	if (world.count || world.node_count != count) ++errors;
	node=world.head;
	for (i=count-1;i>=0;--i) {
		if (!node || ((void **)node)[0] != &countries[i] || countries[i].payload != i+1 || countries[i].heavy_marks != 1) { ++errors; break; }
		node=((void **)node)[2];
	}
	if (node) ++errors;
	snprintf(text,sizeof(text),"errors=%u\nops-allocations=%u\n",errors,hits());
	da_abi_proof("daily-large-ops.txt",text);
	HeapFree(GetProcessHeap(),0,countries); HeapFree(GetProcessHeap(),0,slots);
	return 0;
}

static int fixture_daily_slice(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef unsigned char *(*BuildFn)(void);
	typedef unsigned int (*SliceFn)(void *, void **, void *, void *, uint64_t, uint64_t, void *);
	BuildFn build = plugin ? (BuildFn)GetProcAddress(plugin, "fixture_daily_build") : NULL;
	SliceFn run = plugin ? (SliceFn)GetProcAddress(plugin, "fixture_daily_slice") : NULL;
	struct fixture_da_world world;
	struct fixture_da_country countries[DA_COUNTRIES];
	void *slots[DA_COUNTRIES];
	struct { int kind; unsigned int index; void *country; } ops[64];
	struct {
		void **snapshot;
		void *shadow, *copy, *game;
		uint64_t from, to;
	} part;
	unsigned char *copy, *shadow;
	unsigned int count, outside = 0, removal_errors = 0, publish_errors = 0;
	unsigned int frame_errors, frame_negative;
	void *worker = plugin ? (void *)GetProcAddress(plugin, "daily_a_worker") : NULL;
	long full = DA_COUNTRIES;
	int i;
	char text[256];
	if (!build || !run || !worker) return 83;
	patch_da_calls(fixture_queue);
	copy = build();
	shadow = VirtualAlloc(NULL, 0x800, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
	if (!copy || !shadow) return 84;
	da_fill(&world, countries, slots, 0);
	memcpy(shadow + 0x300 + 0x1C8, &world.array, sizeof(world.array));
	memcpy(shadow + 0x300 + 0x1D4, &full, sizeof(full));
	count = run(&world, slots, shadow, copy, 4, 9, ops);
	for (i = 0; i < DA_COUNTRIES; ++i)
		if (i < 4 || i >= 9) outside += countries[i].heavy_marks;
	if (count == 6) {
		unsigned int at;
		static const unsigned int indices[] = {8,7,6,6,5,4};
		static const int kinds[] = {1,1,2,3,1,1};
		for (at = 0; at < count; ++at) {
			if (ops[at].kind != kinds[at]) { ++removal_errors; ++publish_errors; continue; }
			if (kinds[at] == 1 && (ops[at].index != indices[at] || ops[at].country != &countries[indices[at]])) ++removal_errors;
			if (kinds[at] == 3 && ops[at].country != &countries[indices[at]]) ++publish_errors;
		}
	} else removal_errors = publish_errors = 99;
	da_fill(&world, countries, slots, 0);
	memcpy(shadow + 0x300 + 0x1D4, &full, sizeof(full));
	part.snapshot = slots; part.shadow = shadow; part.copy = copy; part.game = &world;
	part.from = 4; part.to = 9;
	frame_errors = fixture_daily_worker_frame(worker, &part);
	frame_negative = fixture_daily_worker_frame((void *)fixture_daily_bad_worker, NULL);
	snprintf(text, sizeof(text), "outside-hits=%u\nops=%u\nremoval-errors=%u\npublish-errors=%u\nframe-errors=%u\nframe-negative=%u\n",
		outside, count, removal_errors, publish_errors, frame_errors, frame_negative);
	da_abi_proof("daily-slice.txt", text);
	if (!VirtualFree(copy, 0, MEM_RELEASE) || !VirtualFree(shadow, 0, MEM_RELEASE)) return 85;
	return 0;
}

static int fixture_daily_header(void) {
	HMODULE plugin = GetModuleHandleA("eu4_country_month.dll");
	typedef void (*ShiftFn)(void *, unsigned int);
	ShiftFn shift = plugin ? (ShiftFn)GetProcAddress(plugin, "daily_a_note_shift") : NULL;
	unsigned char header[0x20] = {0};
	int countries[3] = {0};
	void *slots[] = {&countries[0], &countries[1], &countries[2]};
	void **array = slots;
	long count = 3;
	char text[96];
	if (!shift) return 86;
	memcpy(header + 8, &array, sizeof(array));
	memcpy(header + 0x14, &count, sizeof(count));
	shift(header, 1);
	memcpy(&count, header + 0x14, sizeof(count));
	snprintf(text, sizeof(text), "count=%ld\nshifted=%d\n", count, slots[1] == &countries[2]);
	da_abi_proof("daily-header.txt", text);
	return 0;
}

static int fixture_daily_list_comparison(void) {
	struct fixture_da_world a = {0}, b = {0};
	struct fixture_da_country ac[DA_COUNTRIES] = {0}, bc[DA_COUNTRIES] = {0};
	void *an[3][3], *bn[3][3];
	int i, same, reordered, short_list, unknown, wrong_count;
	char text[192];
	for (i = 0; i < 3; ++i) {
		an[i][0] = &ac[2-i]; bn[i][0] = &bc[2-i];
		an[i][1] = i ? an[i-1] : NULL; bn[i][1] = i ? bn[i-1] : NULL;
		an[i][2] = i < 2 ? an[i+1] : NULL; bn[i][2] = i < 2 ? bn[i+1] : NULL;
	}
	a.head = an[0]; b.head = bn[0]; a.tail = an[2]; b.tail = bn[2];
	a.node_count = b.node_count = 3;
	same = da_list_matches(&a, &b, ac, bc);
	bn[0][0] = &bc[1]; bn[1][0] = &bc[2];
	reordered = da_list_matches(&a, &b, ac, bc);
	bn[0][0] = &bc[2]; bn[1][0] = &bc[1];
	bn[1][2] = NULL;
	short_list = da_list_matches(&a, &b, ac, bc);
	bn[1][2] = bn[2]; bn[1][0] = NULL;
	unknown = da_list_matches(&a, &b, ac, bc);
	bn[1][0] = &bc[1]; b.node_count = 4;
	wrong_count = da_list_matches(&a, &b, ac, bc);
	snprintf(text, sizeof(text), "same=%d\nreordered=%d\nshort-list=%d\nunknown=%d\nwrong-count=%d\n",
		same, reordered, short_list, unknown, wrong_count);
	da_abi_proof("daily-list-comparison.txt", text);
	return 0;
}

static int fixture_drive_daily_a(int serial_only) {
	AddVectoredExceptionHandler(1, da_veh);
	struct fixture_da_world serial_world, warm_world, parallel_world;
	struct fixture_da_country serial_c[DA_COUNTRIES], warm_c[DA_COUNTRIES], parallel_c[DA_COUNTRIES];
	void *serial_slots[DA_COUNTRIES], *warm_slots[DA_COUNTRIES], *parallel_slots[DA_COUNTRIES];
	unsigned char *baseline;
	typedef void (*QueueFn)(void *);
	typedef unsigned long long (*ShimFn)(void *, void *);
	ShimFn shim = da_shim;
	int i;
	int payloads_ok = 1;
	int warm_payloads_ok = 1, warm_match = 0, warm_workers = 0;
	long warm_calls = 0;
	int rc = 0;
	HANDLE file;
	DWORD wrote = 0;
	char proof[512];
	if (serial_only) {
		unsigned long long alignment = da_shim((void *)fixture_daily_stack_probe, NULL);
		snprintf(proof, sizeof(proof), "entry-rsp-mod16=%llu\n", alignment);
		da_abi_proof("daily-a-serial-abi.txt", proof);
		if (alignment != 8) return 64;
	}

	patch_da_calls(fixture_queue);
	baseline = exec_near((void *)fixture_queue, DAILY_A_QUEUE_BYTES + 0x40);
	if (!baseline) return 60;
	memcpy(baseline, fixture_queue, DAILY_A_QUEUE_BYTES);
	{
		static const unsigned char head[21] = {
			0x48,0x8B,0x50,0x08,0x4C,0x8B,0x3C,0xCA,0x4C,0x89,0xBD,0xA8,0x00,0x00,0x00,
			0x4C,0x89,0xBD,0xF8,0x00,0x00};
		memcpy(baseline + 0x60, head, sizeof(head));
		{
			static const unsigned char epilogue[] = {0x48,0x81,0xC4,0x08,0x02,0,0,0x41,0x5F,0x41,0x5E,0x41,0x5D,0x41,0x5C,0x5F,0x5E,0x5B,0x5D,0xC3};
			memcpy(baseline + 0x14A2, epilogue, sizeof(epilogue));
		}
		FlushInstructionCache(GetCurrentProcess(), baseline, DAILY_A_QUEUE_BYTES);
	}

	patch_da_calls(baseline);
	{ int q; char bb[128]; int bl = 0;
		bl = sprintf(bb, "post-patch 0x150: " );
		for (q = 0x150; q < 0x170; ++q) bl += sprintf(bb + bl, "%02x", baseline[q]);
		bl += sprintf(bb + bl, "\n");
		fprintf(stderr, "%s", bb); fflush(stderr);
	}

	da_fill(&serial_world, serial_c, serial_slots, 0);
	fprintf(stderr, "da-stage=serial\n"); fflush(stderr);
	fprintf(stderr, "baseline=%p queue=%p\n", baseline, (void *)fixture_queue); fflush(stderr);
	if (serial_only) shim(baseline, &serial_world);
	else {
		if (!tf_begin("foch-host\\steps.txt", baseline)) return 67;
		fixture_trace_invoke(baseline, &serial_world, tf_guard);
		if (!tf_end()) return 68;
	}
	fprintf(stderr, "post-serial count=%ld nodes=%ld\n", serial_world.count, serial_world.node_count); fflush(stderr);
	if (serial_only) {
		void *node = serial_world.head;
		int ordered = 1;
		for (i = DA_COUNTRIES - 1; i >= 0; --i) {
			int heavy = (i % 3) == 0;
			if (!node || ((void **)node)[0] != &serial_c[i]) { ordered = 0; break; }
			if (serial_c[i].payload != i + heavy || serial_c[i].heavy_marks != heavy) payloads_ok = 0;
			node = ((void **)node)[2];
		}
		if (node) ordered = 0;
		snprintf(proof, sizeof(proof),
			"entry-rsp-mod16=8\nserial-count=%ld\nserial-nodes=%ld\nordered=%d\npayloads=%d\n",
			serial_world.count, serial_world.node_count, ordered, payloads_ok);
		da_abi_proof("daily-a-serial-abi.txt", proof);
		VirtualFree(baseline, 0, MEM_RELEASE);
		return 0;
	}
	fprintf(stderr, "da-stage=warm\n"); fflush(stderr);
	/* First hooked call warms up on one thread; compare the second. */
	da_fill(&warm_world, warm_c, warm_slots, 0);
	shim(fixture_queue, &warm_world);
	{
		FILE *metrics = fopen("foch-host/daily-a-real.txt", "r");
		char line[96];
		if (metrics) {
			while (fgets(line, sizeof(line), metrics)) {
				sscanf(line, "calls=%ld", &warm_calls);
				sscanf(line, "workers=%d", &warm_workers);
			}
			fclose(metrics);
		}
	}
	fprintf(stderr, "da-stage=parallel\n"); fflush(stderr);
	da_fill(&parallel_world, parallel_c, parallel_slots, 0);
	shim(fixture_queue, &parallel_world);
	fprintf(stderr, "da-stage=done\n"); fflush(stderr);

	for (i = 0; i < DA_COUNTRIES; ++i) {
		if (serial_c[i].payload != parallel_c[i].payload || serial_c[i].heavy_marks != parallel_c[i].heavy_marks) payloads_ok = 0;
		if (serial_c[i].payload != warm_c[i].payload || serial_c[i].heavy_marks != warm_c[i].heavy_marks) warm_payloads_ok = 0;
	}
	warm_match = da_list_matches(&serial_world, &warm_world, serial_c, warm_c) &&
		warm_world.count == serial_world.count && warm_payloads_ok && warm_calls == 1 && warm_workers == 1;
	if (!da_list_matches(&serial_world, &parallel_world, serial_c, parallel_c)) rc = 61;
	else if (!payloads_ok) rc = 62;
	else if (parallel_world.count != serial_world.count) rc = 63;
	else if (!warm_match) rc = 65;
		{
		const char *names[] = {"daily-a-dispatch.txt", "daily-a-real.txt"};
		int ni;
		for (ni = 0; ni < 2; ++ni) {
			FILE *f = fopen(names[ni], "r");
			fprintf(stderr, "== %s ==\n", names[ni]);
			if (f) {
				char line[128];
				while (fgets(line, sizeof(line), f)) fputs(line, stderr);
				fclose(f);
			} else fprintf(stderr, "(absent)\n");
			fflush(stderr);
		}
	}
snprintf(proof, sizeof(proof),
		"match=%s\nserial-count=%ld\nparallel-count=%ld\nserial-nodes=%ld\nparallel-nodes=%ld\npayloads=%s\nwarm-match=%d\nwarm-calls=%ld\nwarm-workers=%d\n",
		rc == 0 ? "ok" : "bad", serial_world.count, parallel_world.count,
		serial_world.node_count, parallel_world.node_count, payloads_ok ? "ok" : "bad", warm_match, warm_calls, warm_workers);
		{
		const char *names[] = {"foch-host\\daily-a-dispatch.txt", "foch-host\\daily-a-real.txt"};
		int ni; int plen = (int)strlen(proof);
		for (ni = 0; ni < 2; ++ni) {
			FILE *f = fopen(names[ni], "r");
			plen += snprintf(proof + plen, sizeof(proof) - plen - 1, "file%d=", ni);
			if (f) {
				char line[96];
				while (fgets(line, sizeof(line), f) && plen < (int)sizeof(proof) - 100)
					plen += snprintf(proof + plen, sizeof(proof) - plen - 1, "%s|", line);
				fclose(f);
			} else plen += snprintf(proof + plen, sizeof(proof) - plen - 1, "absent\n");
		}
	}
file = CreateFileA("foch-host\\daily-a-fixture.txt", GENERIC_WRITE, FILE_SHARE_READ, NULL,
		CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (file != INVALID_HANDLE_VALUE) {
		WriteFile(file, proof, (DWORD)strlen(proof), &wrote, NULL);
		CloseHandle(file);
	}
	printf("%s", proof);
	VirtualFree(baseline, 0, MEM_RELEASE);
	return rc;
}

/* Failed member initialization must leave the real native loops intact. */
static int fixture_native_members(void) {
    HMODULE plugin=GetModuleHandleA("eu4_country_month.dll");
    volatile LONG *done=plugin?(volatile LONG *)GetProcAddress(plugin,"states_done"):NULL;
    struct fixture_da_world world;struct fixture_da_country countries[DA_COUNTRIES];void *slots[DA_COUNTRIES];
    void *begin=g_ptrs,*end=g_ptrs+FIXTURE_COUNTRIES;DWORD caller=GetCurrentThreadId();int i,errors=0;char text[256];
    memset(g_world,0,sizeof(g_world));memset(g_countries,0,sizeof(g_countries));memset(g_states,0,sizeof(g_states));g_hits=g_hits_at_after=0;
    for(i=0;i<FIXTURE_COUNTRIES;i++) {int j;g_ptrs[i]=&g_countries[i];for(j=0;j<STATES_EACH;j++)g_state_ptrs[i][j]=&g_states[i][j];g_countries[i].states=g_state_ptrs[i];g_countries[i].states_end=g_state_ptrs[i]+STATES_EACH;}
    memcpy(g_world+0x1ce8,&begin,8);memcpy(g_world+0x1cf0,&end,8);
    if(done)*done=1; /* Any residual state skip would now lose native work. */
    fixture_run_countries(g_world,0,FIXTURE_COUNTRIES,0xA5,0x3C);
    if(done)*done=0;
    for(i=0;i<FIXTURE_COUNTRIES;i++) {int j;if(g_countries[i].hits!=1||g_countries[i].tid!=caller||g_countries[i].dl!=0x3C||g_countries[i].r8!=0xA5)errors++;
        for(j=0;j<STATES_EACH;j++)if(g_states[i][j].hits!=1||g_states[i][j].tid!=caller)errors++;}
    if(g_hits_at_after!=FIXTURE_COUNTRIES)errors++;
    if(fixture_drive_active())errors++;
    patch_da_calls(fixture_queue);da_fill(&world,countries,slots,0);da_shim(fixture_queue,&world);
    {void *node=world.head;int index=0;while(node&&index<DA_COUNTRIES) {int n=DA_COUNTRIES-index-1;if(((void **)node)[0]!=&countries[n]||countries[n].heavy_marks!=(n%3==0)||countries[n].payload!=n+(n%3==0))errors++;node=((void **)node)[2];index++;}if(node||index!=DA_COUNTRIES||world.count)errors++;}
    snprintf(text,sizeof(text),"errors=%d\ncountries=%d\nstates=%d\ndaily=%d\n",errors,FIXTURE_COUNTRIES,FIXTURE_COUNTRIES*STATES_EACH,DA_COUNTRIES);da_abi_proof("init-native.txt",text);
    return errors?117:0;
}

/* Same-length effects in the exact regions bypassed by the detours. */
static int fixture_profile_native_patch(int monthly) {
    static const unsigned char month[]={0x41,0xFF,0x87,0x00,0x01,0x00,0x00};
    static const unsigned char active[]={0xFF,0x45,0x7C,0x90,0x90};
    unsigned char *at=(monthly?fixture_country_site():fixture_active_site())+(monthly?0x18:0x20);
    size_t bytes=monthly?sizeof(month):sizeof(active);DWORD old;
    if(!VirtualProtect(at,bytes,PAGE_EXECUTE_READWRITE,&old))return 118;
    memcpy(at,monthly?month:active,bytes);
    if(!VirtualProtect(at,bytes,old,&old)||!FlushInstructionCache(GetCurrentProcess(),at,bytes))return 119;
    return 0;
}
static int fixture_profile_monthly(void) {
    void *begin=g_ptrs,*end=g_ptrs+FIXTURE_COUNTRIES;DWORD caller=GetCurrentThreadId();
    int i,country_hits=0,state_hits=0,caller_states=0,args=1;char text[256];HANDLE file;DWORD wrote;
    memset(g_world,0,sizeof(g_world));memset(g_countries,0,sizeof(g_countries));memset(g_states,0,sizeof(g_states));g_hits=g_hits_at_after=0;
    for(i=0;i<FIXTURE_COUNTRIES;i++) {int j;g_ptrs[i]=&g_countries[i];for(j=0;j<STATES_EACH;j++)g_state_ptrs[i][j]=&g_states[i][j];g_countries[i].states=g_state_ptrs[i];g_countries[i].states_end=g_state_ptrs[i]+STATES_EACH;}
    memcpy(g_world+0x1ce8,&begin,8);memcpy(g_world+0x1cf0,&end,8);
    fixture_run_countries(g_world,0,FIXTURE_COUNTRIES,0xA5,0x3C);
    for(i=0;i<FIXTURE_COUNTRIES;i++) {int j;country_hits+=g_countries[i].hits;
        if(g_countries[i].hits!=1||g_countries[i].tid!=caller||g_countries[i].dl!=0x3C||g_countries[i].r8!=0xA5)args=0;
        for(j=0;j<STATES_EACH;j++) {state_hits+=g_states[i][j].hits;caller_states+=g_states[i][j].hits==1&&g_states[i][j].tid==caller;}}
    snprintf(text,sizeof(text),"tail-counter=%u\ncountries=%d\nstates=%d\ncaller-states=%d\nargs-ok=%d\n",*(uint32_t *)(g_world+0x100),country_hits,state_hits,caller_states,args);
    file=CreateFileA("foch-host\\profile-monthly.txt",GENERIC_WRITE,FILE_SHARE_READ,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(file==INVALID_HANDLE_VALUE)return 120;WriteFile(file,text,(DWORD)strlen(text),&wrote,NULL);CloseHandle(file);return 0;
}

struct monthly_frame {
    void *entry,*world; uint64_t from,end; int32_t initial_edi; uint32_t pad;
    uint64_t out[16]; unsigned char xmm[96];
    uint64_t helper_r8,helper_r10,helper_r11,helper_fifth,helper_rsp;
};
typedef char monthly_out_offset[offsetof(struct monthly_frame,out)==40?1:-1];
typedef char monthly_xmm_offset[offsetof(struct monthly_frame,xmm)==168?1:-1];
struct monthly_frame *fixture_monthly_current;
extern void fixture_monthly_invoke(struct monthly_frame *frame);
extern void fixture_monthly_join(void);
extern void fixture_monthly_primary_bridge(void);
extern void fixture_monthly_helper_bridge(void);
static void *monthly_state_entry;
static int monthly_events[128],monthly_event_count,monthly_primary_count,monthly_arg_errors;
static uint64_t monthly_r9[64];
void fixture_monthly_primary_record(void *country,uint64_t dl,uint64_t r8,uint64_t r9) {
    struct fixture_country *c=country;int index=(int)(c-g_countries),i;
    monthly_events[monthly_event_count++]=index+1;monthly_r9[monthly_primary_count++]=r9;
    if(index<0||index>=FIXTURE_COUNTRIES||dl!=0x3C||r8!=0xA5)monthly_arg_errors++;
    c->hits++;c->tid=GetCurrentThreadId();c->dl=(unsigned char)dl;c->r8=(unsigned char)r8;
    for(i=0;i<STATES_EACH;i++)((void(*)(void *))monthly_state_entry)(&g_states[index][i]);
}
void fixture_monthly_helper_record(void *game,uint64_t zero,uint64_t r8,uint64_t r9) {
    struct monthly_frame *f=fixture_monthly_current;
    int last=monthly_event_count?monthly_events[monthly_event_count-1]:0;
    monthly_events[monthly_event_count++]=-last;
    if(game!=g_world||zero||r9!=0x10F8||r8!=0x88888888888888A5ull||
        f->helper_r10!=0xAAAAAAAAAAAAAA10ull||f->helper_r11!=0xBBBBBBBBBBBBBB11ull||
        f->helper_fifth!=0x5555666677778888ull||(f->helper_rsp&15)!=8)monthly_arg_errors++;
}
static void fixture_monthly_reset(struct monthly_frame *f,void *image,uint64_t from,uint64_t end,int edi) {
    void *begin=g_ptrs,*finish=g_ptrs+FIXTURE_COUNTRIES;int i;
    memset(f,0,sizeof(*f));f->entry=(unsigned char *)image+0x75DD30;f->world=g_world;f->from=from;f->end=end;f->initial_edi=edi;
    fixture_monthly_current=f;monthly_state_entry=(unsigned char *)image+0x1000;
    memset(g_world,0,sizeof(g_world));memset(g_countries,0,sizeof(g_countries));memset(g_states,0,sizeof(g_states));
    for(i=0;i<FIXTURE_COUNTRIES;i++){int j;g_ptrs[i]=&g_countries[i];for(j=0;j<STATES_EACH;j++)g_state_ptrs[i][j]=&g_states[i][j];g_countries[i].states=g_state_ptrs[i];g_countries[i].states_end=g_state_ptrs[i]+STATES_EACH;}
    memcpy(g_world+0x1CE8,&begin,8);memcpy(g_world+0x1CF0,&finish,8);
    monthly_event_count=monthly_primary_count=monthly_arg_errors=0;memset(monthly_events,0,sizeof(monthly_events));memset(monthly_r9,0,sizeof(monthly_r9));
}
static int fixture_monthly_callback(const char *mode) {
    HMODULE dll=LoadLibraryA("plugins/eu4_country_month.dll");
    typedef int(*ImageFn)(void *,void *,void *,void *,void *);typedef int(*InstallFn)(void *);typedef void(*FaultFn)(const char *,unsigned);typedef unsigned(*HitsFn)(void);
    ImageFn image_fn=dll?(ImageFn)GetProcAddress(dll,"fixture_monthly_image"):NULL;
    InstallFn install=dll?(InstallFn)GetProcAddress(dll,"fixture_monthly_install"):NULL;
    FaultFn fault=dll?(FaultFn)GetProcAddress(dll,"fixture_daily_fault"):NULL;
    HitsFn hits=dll?(HitsFn)GetProcAddress(dll,"fixture_daily_fault_hits"):NULL;
    unsigned char *native=VirtualAlloc(NULL,0x25E0000,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READWRITE);
    unsigned char *hooked=VirtualAlloc(NULL,0x25E0000,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READWRITE);
    const uint64_t from[]={0,3,1},end[]={32,29,17};const int edi[]={0,7,-8};int variant,errors=0,native_first=0,hook_first=0;
    char text[512];
    if(!image_fn||!install||!fault||!hits||!native||!hooked)return 124;
    if(image_fn(native,fixture_monthly_primary_bridge,fixture_monthly_helper_bridge,fixture_state_body,fixture_monthly_join)||
       image_fn(hooked,fixture_monthly_primary_bridge,fixture_monthly_helper_bridge,fixture_state_body,fixture_monthly_join)||install(hooked))return 125;
    for(variant=0;variant<3;variant++) {
        struct monthly_frame a,b;int events[128],n,primary,i,helper,caller_states=0;uint64_t r9[64];int native_args;unsigned fault_hits;
        int fallback=!strcmp(mode,"monthly-callback-parts")||!strcmp(mode,"monthly-callback-pool");int work_failure=!strcmp(mode,"monthly-callback-work");
        fixture_monthly_reset(&a,native,from[variant],end[variant],edi[variant]);fixture_monthly_invoke(&a);
        n=monthly_event_count;primary=monthly_primary_count;native_args=monthly_arg_errors;memcpy(events,monthly_events,sizeof(events));memcpy(r9,monthly_r9,sizeof(r9));
        helper=*(int *)(g_world+0x100);if(!variant)native_first=helper;
        fixture_monthly_reset(&b,hooked,from[variant],end[variant],edi[variant]);
        fault(!strcmp(mode,"monthly-callback-parts")?"country-parts":!strcmp(mode,"monthly-callback-pool")?"country-pool":!strcmp(mode,"monthly-callback-work")?"country-work":NULL,1);fixture_monthly_invoke(&b);fault_hits=hits();fault(NULL,0);
        if(!variant)hook_first=*(int *)(g_world+0x100);
        if(helper!=*(int *)(g_world+0x100)||native_args||monthly_arg_errors||n!=monthly_event_count||primary!=monthly_primary_count||memcmp(events,monthly_events,n*sizeof(int))||memcmp(r9,monthly_r9,primary*sizeof(uint64_t)))errors++;
        if(memcmp(a.out,b.out,sizeof(a.out))||memcmp(a.xmm,b.xmm,sizeof(a.xmm)))errors++;
        if(a.out[8]!=(uint32_t)(edi[variant]+(int)(end[variant]-from[variant]))||a.out[9]!=end[variant])errors++;
        if(a.out[10]!=end[variant]||a.out[11]!=0x12121212121212A5ull||a.out[12]!=0x131313131313133Cull||a.out[13]!=0x1111222233334444ull||a.out[14]!=(uintptr_t)g_world||a.out[15]!=0x5555666677778888ull)errors++;
        if(helper!=(variant==0?7:variant==1?5:3))errors++;
        if((fallback||work_failure)&&!fault_hits)errors++;
        for(i=0;i<FIXTURE_COUNTRIES;i++){int j,wanted=(uint64_t)i>=from[variant]&&(uint64_t)i<end[variant];
            if(g_countries[i].hits!=wanted)errors++;
            for(j=0;j<STATES_EACH;j++){if(g_states[i][j].hits!=wanted)errors++;if(wanted&&g_states[i][j].tid==GetCurrentThreadId())caller_states++;}
        }
        if((fallback||!strcmp(mode,"monthly-callback-serial"))&&caller_states!=2*(int)(end[variant]-from[variant]))errors++;
        if(!strcmp(mode,"monthly-callback-parallel")&&caller_states)errors++;
        if(work_failure&&(caller_states==0||caller_states==2*(int)(end[variant]-from[variant])))errors++;
    }
    snprintf(text,sizeof(text),"native-first=%d\nhook-first=%d\nvariants=3\nerrors=%d\n",native_first,hook_first,errors);da_abi_proof("monthly-callback.txt",text);
    VirtualFree(native,0,MEM_RELEASE);VirtualFree(hooked,0,MEM_RELEASE);return 0;
}

int main(int argc, char **argv) {
	wchar_t system[MAX_PATH];
	wchar_t sample[MAX_PATH];
	HMODULE proxy = GetModuleHandleW(L"version.dll");
	HMODULE real;
	DWORD ignored = 0, proxy_size, real_size;
	typedef DWORD (WINAPI *SizeFn)(LPCWSTR, LPDWORD);
	unsigned i;
	stage(L"foch-host\\game-main-entered");
	if(argc>1&&!strncmp(argv[1],"monthly-callback-",17)) {
		int month_rc=fixture_monthly_callback(argv[1]);if(month_rc)return month_rc;
	}
	if(argc>1&&!strncmp(argv[1],"profile-",8)) {
		int monthly=!strncmp(argv[1],"profile-monthly-",16),profile_rc;
		if(strstr(argv[1],"-native")) {profile_rc=fixture_profile_native_patch(monthly);if(profile_rc)return profile_rc;}
		profile_rc=monthly?fixture_profile_monthly():fixture_drive_active();if(profile_rc)return profile_rc;
	}
	if (argc > 1 && !strncmp(argv[1],"init-fault-",11) && strcmp(argv[1],"init-fault-active-enable-rollback")) {
		int native_rc=fixture_native_members();if(native_rc)return native_rc;
	}
	if (argc > 1 && (!strncmp(argv[1],"member-edge-",12) || !strcmp(argv[1],"member-active-extra") || !strcmp(argv[1],"member-known-profiles"))) {
		HMODULE member = LoadLibraryA("plugins\\eu4_country_month.dll");
		typedef int (*EdgeFn)(unsigned int);
		typedef int (*ExtraFn)(void);
		if (!member) return 114;
		if (!strcmp(argv[1],"member-active-extra") || !strcmp(argv[1],"member-known-profiles")) {
			ExtraFn run = (ExtraFn)GetProcAddress(member,!strcmp(argv[1],"member-known-profiles")?"fixture_known_production_profiles":"fixture_active_extra_call");
			if (!run || run()) return 115;
		} else {
			EdgeFn run = (EdgeFn)GetProcAddress(member,"fixture_member_edges");
			unsigned int kind = !strcmp(argv[1],"member-edge-monthly")?0:!strcmp(argv[1],"member-edge-active")?1:2;
			if (!run || run(kind)) return 116;
		}
	}
	if (argc > 1 && strcmp(argv[1], "countries") == 0) {
		int country_rc = fixture_drive_countries();
		if (country_rc != 0) return country_rc;
	}
	if (argc > 1 && strcmp(argv[1], "active") == 0) {
		int active_rc = fixture_drive_active();
		if (active_rc != 0) return active_rc;
	}
	if (argc > 1 && strcmp(argv[1], "active-timing") == 0) {
		int timing_rc = fixture_time_active();
		if (timing_rc != 0) return timing_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya") == 0) {
		int da_rc = fixture_drive_daily_a(0);
		if (da_rc != 0) return da_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-serial") == 0) {
		int da_rc = fixture_drive_daily_a(1);
		if (da_rc != 0) return da_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-fallback") == 0) {
		int da_rc = fixture_daily_fallback_abi();
		if (da_rc != 0) return da_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-trace") == 0) {
		int trace_rc = fixture_trace_lifecycle();
		if (trace_rc != 0) return trace_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-trace-unowned") == 0) {
		int trace_rc = fixture_trace_unowned();
		if (trace_rc != 0) return trace_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-worker-abi") == 0) {
		int worker_rc = fixture_daily_worker_abi();
		if (worker_rc != 0) return worker_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-build") == 0) {
		int builder_rc = fixture_daily_builder();
		if (builder_rc != 0) return builder_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-slice") == 0) {
		int slice_rc = fixture_daily_slice();
		if (slice_rc != 0) return slice_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-home") == 0) {
		fixture_daily_home = 1;
		if (fixture_daily_slice() != 0) return 93;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-relocation") == 0) {
		int relocation_rc = fixture_daily_relocation();
		if (relocation_rc != 0) return relocation_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-arena-owner") == 0 && fixture_daily_arena(0) != 0) return 95;
	if (argc > 1 && strcmp(argv[1], "dailya-arena-rounds") == 0 && fixture_daily_rounds() != 0) return 97;
	if (argc > 1 && strcmp(argv[1], "dailya-range") == 0 && fixture_daily_range() != 0) return 103;
	if (argc > 1 && strncmp(argv[1], "dailya-fail-", 12) == 0 && fixture_daily_failure(argv[1]+12) != 0) return 105;
	if (argc > 1 && strcmp(argv[1],"dailya-large-ops") == 0 && fixture_daily_large_ops() != 0) return 107;
	if (argc > 1 && strcmp(argv[1], "dailya-arena-reuse") == 0 && fixture_daily_arena(1) != 0) return 95;
	if (argc > 1 && strcmp(argv[1], "dailya-arena-boundary") == 0 && fixture_daily_arena(2) != 0) return 95;
	if (argc > 1 && strcmp(argv[1], "dailya-header") == 0) {
		int header_rc = fixture_daily_header();
		if (header_rc != 0) return header_rc;
	}
	if (argc > 1 && strcmp(argv[1], "dailya-compare") == 0) {
		int compare_rc = fixture_daily_list_comparison();
		if (compare_rc != 0) return compare_rc;
	}
	if (argc > 1 && strcmp(argv[1], "hooks") == 0) {
		HMODULE hooks;
		typedef int (*ObserveFn)(void);
		ObserveFn observe;
		if (fixture_hookable() != 7) return 20;
		hooks = GetModuleHandleW(L"hooks.dll");
		observe = hooks ? (ObserveFn)GetProcAddress(hooks, "fixture_observe_flow") : NULL;
		if (!observe || observe() != 0) return 21;
		/* Keep both ambiguous sites live for the pattern lookup. */
		(void)fixture_ambiguous_site();
	}
	GetSystemDirectoryW(system, MAX_PATH);
	swprintf_s(sample, MAX_PATH, L"%s\\kernel32.dll", system);
	wcscat_s(system, MAX_PATH, L"\\version.dll");
	if (!proxy) return 1;
	if (argc > 1 && strcmp(argv[1], "proxy-threads") == 0) {
		HANDLE threads[8];
		for (i = 0; i < 8; ++i) {
			threads[i] = CreateThread(NULL, 0, first_version_call, sample, 0, NULL);
			if (!threads[i]) return 10;
		}
		if (WaitForMultipleObjects(8, threads, TRUE, 10000) != WAIT_OBJECT_0) return 11;
		for (i = 0; i < 8; ++i) {
			DWORD result;
			if (!GetExitCodeThread(threads[i], &result) || result) return 12;
			CloseHandle(threads[i]);
		}
	}
	proxy_size = GetFileVersionInfoSizeW(sample, &ignored);
	stage(L"foch-host\\game-version-resolved");
	real = LoadLibraryExW(system, NULL, LOAD_LIBRARY_SEARCH_SYSTEM32);
	if (!real) return 1;
	/* Each forwarding stub must tail-jump to the corresponding system export. */
	for (i = 0; i < sizeof(exports) / sizeof(exports[0]); ++i) {
		const unsigned char *stub = (const unsigned char *)GetProcAddress(proxy, exports[i]);
		int32_t offset;
		FARPROC target;
		if (!stub || stub[0] != 0xff || stub[1] != 0x25) return 2;
		memcpy(&offset, stub + 2, sizeof(offset));
		memcpy(&target, stub + 6 + offset, sizeof(target));
		if (target != GetProcAddress(real, exports[i])) return 3;
		if (GetProcAddress(proxy, MAKEINTRESOURCEA(i + 1)) != (FARPROC)stub) return 7;
		if (GetProcAddress(real, MAKEINTRESOURCEA(i + 1)) != target) return 8;
	}
	real_size = ((SizeFn)GetProcAddress(real, "GetFileVersionInfoSizeW"))(sample, &ignored);
	if (!proxy_size || proxy_size != real_size) return 4;
	if (GetModuleHandleW(L"unexpected.dll") || GetModuleHandleW(L"auxiliary.dll")) return 5;
	if (argc > 1 && strcmp(argv[1], "native") == 0 && !native_at_constructor) return 6;
	/* The Rust harness acknowledges terminal events; elapsed time is not proof. */
	if (argc > 1 && strcmp(argv[1], "forward-only") != 0 && strcmp(argv[1], "proxy-threads") != 0) {
		ULONGLONG deadline = GetTickCount64() + 10000;
		while (GetFileAttributesW(L"foch-host\\fixture-complete") == INVALID_FILE_ATTRIBUTES) {
			if (GetTickCount64() >= deadline) return 9;
			Sleep(10);
		}
	}
	puts("entry reached; all 17 VERSION targets and version API match system32");
	return 0;
}
