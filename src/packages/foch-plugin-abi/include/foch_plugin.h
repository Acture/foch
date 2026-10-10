/*
 * foch_plugin.h — the Foch native plugin interface, ABI 1.0.
 *
 * A plugin is a Windows x64 DLL that Foch's in-game host loads into eu4.exe
 * according to the frozen load plan of one launch. The plugin exports exactly
 * one function, foch_plugin_get_api, and receives host services through a
 * FochHostApi table of its own.
 *
 * Rules (see the design note for the rationale):
 * - Every struct starts with `size`; whoever fills a struct writes sizeof.
 *   Minor versions only append fields, so read a field only when `size`
 *   covers it. A different major version is never loaded.
 * - Strings are UTF-8 with an explicit length and need no NUL terminator.
 *   Memory is never freed across the boundary: the host owns what it hands
 *   out, the plugin owns what it returns.
 * - foch_plugin_get_api has no side effects. The host never calls into the
 *   plugin from DllMain; keep the plugin's own DllMain minimal as well.
 * - Every FochHostApi function is thread-safe.
 */
#ifndef FOCH_PLUGIN_H
#define FOCH_PLUGIN_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FOCH_ABI_MAJOR 1
#define FOCH_ABI_MINOR 0

typedef struct FochStr {
	const char *ptr;
	size_t len;
} FochStr;

/* Results of host services and plugin entry points. */
typedef enum FochResult {
	FOCH_OK = 0,
	FOCH_E_INVALID = -1,     /* bad argument or call in the wrong phase */
	FOCH_E_UNSUPPORTED = -2, /* e.g. a hook target whose prologue cannot be relocated */
	FOCH_E_CONFLICT = -3,    /* overlaps another plugin's patch, or foreign code already patched it */
	FOCH_E_MEMORY = -4,      /* no executable memory within reach, or protection change failed */
	FOCH_E_NOT_FOUND = -5,
	FOCH_E_AMBIGUOUS = -6    /* a pattern matched more than once */
} FochResult;

/* Actual state a plugin reports about itself. */
typedef enum FochState {
	FOCH_STATE_INITIALIZING = 1,
	FOCH_STATE_ACTIVE = 2,
	FOCH_STATE_INACTIVE = 3, /* loaded, but not in effect by its configuration */
	FOCH_STATE_REFUSED = 4,  /* its own compatibility checks declined */
	FOCH_STATE_FAILED = 5
} FochState;

/* Capability bits a plugin declares in FochPluginApi.caps. */
#define FOCH_CAP_LIVE_CONFIG (1ull << 0) /* accepts config_changed while running */
#define FOCH_CAP_LIVE_TOGGLE (1ull << 1) /* accepts set_enabled while running */
#define FOCH_CAP_SHUTDOWN (1ull << 2)    /* wants a shutdown call on orderly exit */

typedef enum FochDirKind {
	FOCH_DIR_DATA = 1,   /* persistent, writable, per plugin */
	FOCH_DIR_CACHE = 2,  /* persistent, writable, may be cleared by Foch */
	FOCH_DIR_LOG = 3,    /* this launch only, collected by Foch */
	FOCH_DIR_PLUGIN = 4  /* the plugin's own files, read-only */
} FochDirKind;

typedef enum FochLogLevel {
	FOCH_LOG_ERROR = 1,
	FOCH_LOG_WARN = 2,
	FOCH_LOG_INFO = 3,
	FOCH_LOG_DEBUG = 4
} FochLogLevel;

typedef struct FochStatus {
	uint32_t size;
	int32_t state;       /* FochState */
	int32_t reason_code; /* plugin-defined, 0 = none */
	FochStr detail;      /* optional; valid until the next status call */
} FochStatus;

/* Opaque handles owned by the host. */
typedef struct FochHook FochHook;
typedef struct FochPatch FochPatch;

typedef struct FochHostApi FochHostApi;
struct FochHostApi {
	uint32_t size;
	uint32_t abi_major;
	uint32_t abi_minor;
	FochStr plugin_id;
	FochStr run_id;
	FochStr game_version; /* e.g. "1.37.5.0" */
	FochStr exe_sha256;   /* diagnostic; never the sole compatibility proof */
	void *exe_base;       /* eu4.exe image base */
	size_t exe_size;      /* eu4.exe SizeOfImage */

	void (*log)(const FochHostApi *host, int32_t level, FochStr message);
	/* Report a state change; asynchronous plugins call this when they finish. */
	void (*report)(const FochHostApi *host, const FochStatus *status);
	/* A directory path; valid for the life of the process. */
	FochStr (*dir)(const FochHostApi *host, int32_t kind);
	/* The configuration snapshot, validated against the manifest schema. */
	FochStr (*config_json)(const FochHostApi *host);
	/* Another plugin's actual state (FochState), or 0 when it is not loaded. */
	int32_t (*plugin_state)(const FochHostApi *host, FochStr plugin_id);

	/*
	 * Shared code services. Every plugin that changes eu4.exe through these
	 * is visible to the host, so overlapping changes are reported instead of
	 * silently corrupting each other.
	 * These slots are reserved in the first-version loading host, which
	 * returns FOCH_E_UNSUPPORTED. Plugins must check service return codes.
	 */

	/* Find the unique match of a pattern such as "48 8B ?? 05 ?" in the
	 * eu4.exe image. FOCH_E_NOT_FOUND / FOCH_E_AMBIGUOUS otherwise. */
	int32_t (*find_pattern)(const FochHostApi *host, FochStr pattern, void **address);

	/*
	 * Register a detour on `target`. Several plugins may hook the same
	 * function: the host chains them, higher `priority` first, ties in load
	 * order. `*next` receives a host-owned cell; always call through
	 * `*(*next)` (re-read it on every call) to reach the next detour or the
	 * original function. Hooks start disabled.
	 */
	int32_t (*hook_create)(const FochHostApi *host, void *target, void *detour,
		int32_t priority, void *const **next, FochHook **hook);
	int32_t (*hook_set_enabled)(const FochHostApi *host, FochHook *hook, int32_t enabled);

	/*
	 * Register a raw byte patch. Patches may not overlap another plugin's
	 * patch or any hooked prologue. Patches start disabled; disabling restores
	 * the original bytes.
	 */
	int32_t (*patch_create)(const FochHostApi *host, void *address, const uint8_t *bytes,
		size_t len, FochPatch **patch);
	int32_t (*patch_set_enabled)(const FochHostApi *host, FochPatch *patch, int32_t enabled);
};

typedef struct FochPluginApi {
	uint32_t size;
	uint32_t abi_major;
	uint32_t abi_minor;
	uint64_t caps;
	/* Called once, in the phase the manifest declares. FOCH_OK means started;
	 * the outcome is reported through status/report. `host` outlives the
	 * process; keep the pointer. */
	int32_t (*init)(const FochHostApi *host);
	void (*status)(FochStatus *out);
	int32_t (*config_changed)(FochStr config_json); /* FOCH_CAP_LIVE_CONFIG only */
	int32_t (*set_enabled)(int32_t enabled);        /* FOCH_CAP_LIVE_TOGGLE only */
	void (*shutdown)(void);                         /* FOCH_CAP_SHUTDOWN only; not on crash */
} FochPluginApi;

#define FOCH_PLUGIN_ENTRY "foch_plugin_get_api"
typedef const FochPluginApi *(*FochPluginGetApi)(uint32_t host_abi_major);

#ifdef __cplusplus
}
#endif

#endif /* FOCH_PLUGIN_H */
