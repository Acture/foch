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
static void before_main(void) {
	HMODULE native = GetModuleHandleW(L"native.dll");
	if (native) {
		typedef int (*ReadyFn)(void);
		ReadyFn ready = (ReadyFn)GetProcAddress(native, "FixtureInitialized");
		native_at_constructor = ready && ready() == 1;
	}
}
#pragma section(".CRT$XCU", read)
__declspec(allocate(".CRT$XCU")) void (__cdecl *fixture_constructor)(void) = before_main;

int main(int argc, char **argv) {
	wchar_t system[MAX_PATH];
	wchar_t sample[MAX_PATH];
	HMODULE proxy = GetModuleHandleW(L"version.dll");
	HMODULE real;
	DWORD ignored = 0, proxy_size, real_size;
	typedef DWORD (WINAPI *SizeFn)(LPCWSTR, LPDWORD);
	unsigned i;
	GetSystemDirectoryW(system, MAX_PATH);
	swprintf_s(sample, MAX_PATH, L"%s\\kernel32.dll", system);
	wcscat_s(system, MAX_PATH, L"\\version.dll");
	real = LoadLibraryExW(system, NULL, LOAD_LIBRARY_SEARCH_SYSTEM32);
	if (!proxy || !real) return 1;
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
	proxy_size = GetFileVersionInfoSizeW(sample, &ignored);
	real_size = ((SizeFn)GetProcAddress(real, "GetFileVersionInfoSizeW"))(sample, &ignored);
	if (!proxy_size || proxy_size != real_size) return 4;
	if (GetModuleHandleW(L"unexpected.dll")) return 5;
	if (argc > 1 && strcmp(argv[1], "native") == 0 && !native_at_constructor) return 6;
	/* Give the bounded fixture polls time to finish, then leave normally. */
	Sleep(500);
	puts("entry reached; all 17 VERSION targets and version API match system32");
	return 0;
}
