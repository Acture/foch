#include <windows.h>

#ifndef FIXTURE_MODE
#define FIXTURE_MODE 0
#endif

static volatile LONG calls;

__declspec(dllexport) int ProbeState(void) {
	if (FIXTURE_MODE == 1) return InterlockedIncrement(&calls) >= 3 ? 1 : 0;
	if (FIXTURE_MODE == 2) return 0;
	if (FIXTURE_MODE == 3) return -3;
	return 1;
}
