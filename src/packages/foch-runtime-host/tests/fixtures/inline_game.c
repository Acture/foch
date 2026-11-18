#include <windows.h>
#include <stdio.h>

extern unsigned long long fixture_inline14(unsigned long long, unsigned long long);
extern unsigned long long fixture_inline15(unsigned long long, unsigned long long);
extern unsigned long long fixture_inline17(unsigned long long, unsigned long long);

int main(void) {
	unsigned long long (*targets[])(unsigned long long, unsigned long long) = {
		fixture_inline14, fixture_inline15, fixture_inline17
	};
	const unsigned lengths[] = {14, 15, 17};
	unsigned i, j;
	DWORD ignored = 0;
	// Import VERSION statically so the actual host runs entry initialization.
	(void)GetFileVersionInfoSizeW(L"eu4.exe", &ignored);
	if (!GetModuleHandleW(L"inline_plugin.dll")) return 1;
	for (i = 0; i < 3; ++i) {
		const unsigned char *bytes = (const unsigned char *)targets[i];
		if (targets[i](7, 11) != 18 || bytes[0] != 0xFF || bytes[1] != 0x25) return 2;
		for (j = 14; j < lengths[i]; ++j) if (bytes[j] != 0x90) return 3;
	}
	puts("SDK chain and stack home area preserved across 14/15/17-byte trampolines");
	return 0;
}
