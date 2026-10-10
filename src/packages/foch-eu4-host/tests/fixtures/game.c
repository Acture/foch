#include <windows.h>
#include <winver.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const char *exports[] = {
	"GetFileVersionInfoA", "GetFileVersionInfoByHandle", "GetFileVersionInfoExA",
	"GetFileVersionInfoExW", "GetFileVersionInfoSizeA", "GetFileVersionInfoSizeExA",
	"GetFileVersionInfoSizeExW", "GetFileVersionInfoSizeW", "GetFileVersionInfoW",
	"VerFindFileA", "VerFindFileW", "VerInstallFileA", "VerInstallFileW",
	"VerLanguageNameA", "VerLanguageNameW", "VerQueryValueA", "VerQueryValueW"
};

static int native_at_constructor;
static void stage(const wchar_t *name) {
	HANDLE file = CreateFileW(name, GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE,
		NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
	if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
}

static void before_main(void) {
	HMODULE native = GetModuleHandleW(L"native.dll");
	stage(L"foch-host\\game-constructor-entered");
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

int main(int argc, char **argv) {
	wchar_t system[MAX_PATH];
	wchar_t sample[MAX_PATH];
	HMODULE proxy = GetModuleHandleW(L"version.dll");
	HMODULE real;
	DWORD ignored = 0, proxy_size, real_size;
	typedef DWORD (WINAPI *SizeFn)(LPCWSTR, LPDWORD);
	unsigned i;
	stage(L"foch-host\\game-main-entered");
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
	if (GetModuleHandleW(L"unexpected.dll")) return 5;
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
