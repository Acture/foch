#include <windows.h>
#include <string.h>
#include <stdlib.h>
#include "foch_plugin.h"

#ifndef FIXTURE_MODE
#define FIXTURE_MODE 0
#endif

static volatile LONG ready;
static volatile LONG status_calls;
static const FochHostApi *saved_host;

__declspec(dllexport) int FixtureInitialized(void) {
	return (int)InterlockedCompareExchange(&ready, 0, 0);
}

static DWORD WINAPI complete_async(void *arg) {
	const FochHostApi *host = (const FochHostApi *)arg;
	FochStatus active = { sizeof(FochStatus), FOCH_STATE_ACTIVE, 0, { "async report", 12 } };
	char delay[16];
	DWORD length = GetEnvironmentVariableA("FOCH_FIXTURE_DELAY_MS", delay, sizeof(delay));
	Sleep(length && length < sizeof(delay) ? strtoul(delay, NULL, 10) : 40);
	InterlockedExchange(&ready, 1);
	// Let polling observe ACTIVE before this malformed callback is submitted.
	if (FIXTURE_MODE == 15) Sleep(200);
	if (FIXTURE_MODE == 14) {
		unsigned char bytes[sizeof(FochStatus) + 1];
		memcpy(bytes + 1, &active, sizeof(active));
		host->report(host, (const FochStatus *)(bytes + 1));
	} else if (FIXTURE_MODE == 15) {
		host->report(host, (const FochStatus *)(uintptr_t)1);
	} else if (FIXTURE_MODE == 16) {
		active.detail = (FochStr){ "\xff", 1 };
		host->report(host, &active);
	} else if (FIXTURE_MODE == 17) {
		SYSTEM_INFO system;
		GetSystemInfo(&system);
		unsigned char *pages = VirtualAlloc(NULL, system.dwPageSize * 2, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
		DWORD old;
		if (!pages || !VirtualProtect(pages + system.dwPageSize, system.dwPageSize, PAGE_NOACCESS, &old)) ExitProcess(8);
		uint32_t *prefix = (uint32_t *)(pages + system.dwPageSize - sizeof(uint32_t));
		*prefix = sizeof(uint32_t);
		host->report(host, (const FochStatus *)prefix);
		VirtualFree(pages, 0, MEM_RELEASE);
	} else {
		host->report(host, &active);
	}
	HANDLE done = CreateFileA("foch-host\\callback-finished-" FIXTURE_NAME, GENERIC_WRITE, FILE_SHARE_READ, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (done != INVALID_HANDLE_VALUE) CloseHandle(done);
	return 0;
}

static int32_t init(const FochHostApi *host) {
	saved_host = host;
	FochStatus active = { sizeof(FochStatus), FOCH_STATE_ACTIVE, 0, { "native init", 11 } };
	FochStr config = host->config_json(host);
	FochStr dir = host->dir(host, FOCH_DIR_PLUGIN);
	if (host->abi_major != 1 || !host->exe_base || !host->exe_size || dir.len == 0)
		return FOCH_E_INVALID;
	if (config.len != 17 || memcmp(config.ptr, "{\"greeting\":\"hi\"}", 17) != 0)
		return FOCH_E_INVALID;
	host->log(host, FOCH_LOG_INFO, (FochStr){ "fixture init", 12 });
	if (FIXTURE_MODE == 4) return FOCH_E_INVALID;
	if (FIXTURE_MODE == 6 || FIXTURE_MODE >= 14) {
		HANDLE thread = CreateThread(NULL, 0, complete_async, (void *)host, 0, NULL);
		if (!thread) return FOCH_E_INVALID;
		CloseHandle(thread);
		return FOCH_OK;
	}
	InterlockedExchange(&ready, 1);
	host->report(host, &active);
	return FOCH_OK;
}

static void status(FochStatus *out) {
	if ((FIXTURE_MODE == 8 || (FIXTURE_MODE >= 10 && FIXTURE_MODE <= 13))
		&& InterlockedIncrement(&status_calls) > 1)
		saved_host->log(saved_host, FOCH_LOG_ERROR, (FochStr){ "polled invalid status", 21 });
	*out = (FochStatus){ FIXTURE_MODE == 8 ? 12 : sizeof(FochStatus),
		FIXTURE_MODE == 8 || (FIXTURE_MODE >= 10 && FIXTURE_MODE <= 13) || !FixtureInitialized()
			? FOCH_STATE_INITIALIZING : FOCH_STATE_ACTIVE,
		0, { "native status", 13 } };
	if (FIXTURE_MODE == 10) out->detail = (FochStr){ NULL, 1 };
	if (FIXTURE_MODE == 11) out->detail = (FochStr){ "\xff", 1 };
	if (FIXTURE_MODE == 12) out->detail = (FochStr){ (const char *)(uintptr_t)1, 1 };
	if (FIXTURE_MODE == 13) out->detail = (FochStr){ "native status", 65537 };
}

static const FochPluginApi api = {
	FIXTURE_MODE == 2 ? 12 : sizeof(FochPluginApi),
	FIXTURE_MODE == 1 ? 9 : FOCH_ABI_MAJOR,
	FOCH_ABI_MINOR, 0,
	FIXTURE_MODE == 3 ? NULL : FIXTURE_MODE == 9 ? (int32_t (*)(const FochHostApi *))&ready : init,
	FIXTURE_MODE == 7 ? NULL : status, NULL, NULL, NULL
};

__declspec(dllexport) const FochPluginApi *foch_plugin_get_api(uint32_t major) {
	(void)major;
	return FIXTURE_MODE == 5 ? NULL : &api;
}
