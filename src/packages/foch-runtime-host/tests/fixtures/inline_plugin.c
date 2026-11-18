#include <windows.h>
#include <stdio.h>
#include "foch_plugin.h"

typedef unsigned long long (*AddFn)(unsigned long long, unsigned long long);
static void *const *next_hi[3];
static void *const *next_lo[3];
static char order[8];
static unsigned order_len;

#define DETOURS(N) \
static unsigned long long hi##N(unsigned long long a, unsigned long long b) { \
	order[order_len++] = 'H'; \
	return ((AddFn)*next_hi[N])(a, b) + 100; \
} \
static unsigned long long lo##N(unsigned long long a, unsigned long long b) { \
	order[order_len++] = 'L'; \
	return ((AddFn)*next_lo[N])(a, b) + 10; \
}
DETOURS(0)
DETOURS(1)
DETOURS(2)

static int32_t init(const FochHostApi *host) {
	const char *names[] = {"fixture_inline14", "fixture_inline15", "fixture_inline17"};
	AddFn hi[] = {hi0, hi1, hi2};
	AddFn lo[] = {lo0, lo1, lo2};
	unsigned i;
	FILE *proof;
	if (host->abi_major != FOCH_ABI_MAJOR) return FOCH_E_INVALID;
	for (i = 0; i < 3; ++i) {
		AddFn target = (AddFn)GetProcAddress(GetModuleHandleW(NULL), names[i]);
		FochHook *low = NULL, *high = NULL;
		FochPatch *patch = NULL;
		const unsigned char replacement = 0xCC;
		if (!target || target(7, 11) != 18) return FOCH_E_INVALID;
		if (host->hook_create(host, (void *)target, (void *)lo[i], 0, &next_lo[i], &low) != FOCH_OK) return FOCH_E_INVALID;
		if (host->hook_set_enabled(host, low, 1) != FOCH_OK) return FOCH_E_INVALID;
		if (host->hook_create(host, (void *)target, (void *)hi[i], 10, &next_hi[i], &high) != FOCH_OK) return FOCH_E_INVALID;
		if (host->hook_set_enabled(host, high, 1) != FOCH_OK) return FOCH_E_INVALID;
		order_len = 0;
		if (target(7, 11) != 128 || order_len != 2 || order[0] != 'H' || order[1] != 'L') return FOCH_E_INVALID;
		if (i > 0 && host->patch_create(host, (unsigned char *)target + 14, &replacement, 1, &patch) != FOCH_E_CONFLICT) return FOCH_E_INVALID;
		if (host->hook_set_enabled(host, high, 0) != FOCH_OK) return FOCH_E_INVALID;
		order_len = 0;
		if (target(7, 11) != 28 || order_len != 1 || order[0] != 'L') return FOCH_E_INVALID;
		if (host->hook_set_enabled(host, low, 0) != FOCH_OK || target(7, 11) != 18) return FOCH_E_INVALID;
	}
	proof = fopen("foch-host/inline-chain.txt", "wb");
	if (!proof) return FOCH_E_INVALID;
	fputs("windows=14,15,17\npriority=HL\ndisable=L,original\narguments=7,11\nresults=128,28,18\nstack-home=preserved\ntail-patch=conflict\n", proof);
	fclose(proof);
	return FOCH_OK;
}

static void status(FochStatus *out) {
	*out = (FochStatus){sizeof(FochStatus), FOCH_STATE_ACTIVE, 0, {NULL, 0}};
}
static const FochPluginApi api = {
	sizeof(FochPluginApi), FOCH_ABI_MAJOR, FOCH_ABI_MINOR, 0, init, status, NULL, NULL, NULL
};
__declspec(dllexport) const FochPluginApi *foch_plugin_get_api(uint32_t major) {
	return major == FOCH_ABI_MAJOR ? &api : NULL;
}
