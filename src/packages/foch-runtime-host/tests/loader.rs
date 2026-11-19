//! Real DLL/EXE fixtures; no installed game or source mod is modified.
#![cfg(all(windows, target_arch = "x86_64"))]

use serde_json::{Value, json};
use std::{
	fs,
	path::{Path, PathBuf},
	process::{Command, Stdio},
	sync::OnceLock,
	time::{Duration, Instant},
};
use tempfile::TempDir;

fn fixtures() -> &'static TempDir {
	static BUILD: OnceLock<TempDir> = OnceLock::new();
	BUILD.get_or_init(|| {
		let root = tempfile::tempdir().unwrap();
		let package = Path::new(env!("CARGO_MANIFEST_DIR"));
		let tool = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "cl.exe")
			.expect("Windows DLL integration tests require the MSVC build tools");
		for (name, source, mode) in [
			("native", "native.c", 0),
			("bad_abi", "native.c", 1),
			("short_api", "native.c", 2),
			("null_init", "native.c", 3),
			("init_error", "native.c", 4),
			("null_api", "native.c", 5),
			("async_native", "native.c", 6),
			("null_status", "native.c", 7),
			("bad_status", "native.c", 8),
			("data_callback", "native.c", 9),
			("null_detail", "native.c", 10),
			("invalid_utf8", "native.c", 11),
			("unreadable_detail", "native.c", 12),
			("oversized_detail", "native.c", 13),
			("unaligned_report", "native.c", 14),
			("unreadable_report", "native.c", 15),
			("invalid_report_detail", "native.c", 16),
			("short_report", "native.c", 17),
			("hooks", "hooks.c", 0),
			("hooks_late", "hooks.c", 1),
			("sync", "legacy.c", 0),
			("poll", "legacy.c", 1),
			("timeout", "legacy.c", 2),
			("refused", "legacy.c", 3),
		] {
			let mut command = tool.to_command();
			command
				.current_dir(root.path())
				.args(["/nologo", "/LD", "/W4", "/utf-8"])
				.arg(format!("/DFIXTURE_MODE={mode}"))
				.arg(format!("/DFIXTURE_NAME=\"{name}\""))
				.arg(format!(
					"/I{}",
					package.join("../foch-plugin-sdk/include").display()
				))
				.arg(package.join("tests/fixtures").join(source))
				.arg(format!("/Fo{name}.obj"))
				.arg(format!("/Fe{name}.dll"));
			let output = command.output().unwrap();
			assert!(
				output.status.success(),
				"{name}: {} {}",
				String::from_utf8_lossy(&output.stdout),
				String::from_utf8_lossy(&output.stderr)
			);
		}
		let ml = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "ml64.exe")
			.expect("Windows DLL integration tests require the MSVC assembler");
		let assembled = ml
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/c", "/Fosites.obj"])
			.arg(package.join("tests/fixtures/sites.asm"))
			.output()
			.unwrap();
		assert!(
			assembled.status.success(),
			"sites: {} {}",
			String::from_utf8_lossy(&assembled.stdout),
			String::from_utf8_lossy(&assembled.stderr)
		);
		let plugin = package.join("../../plugins/eu4-country-month");
		// Keep the probe in the fixture translation unit so the production DLL
		// does not gain test exports or an alternate body builder.
		let builder_probe = root.path().join("country_fixture.c");
		fs::write(
			&builder_probe,
			format!(
				concat!(
					"#include <windows.h>\n#include <string.h>\n",
					"static void *fixture_fail_heap(const char *, DWORD, size_t); static void *fixture_fail_virtual(const char *, void *, size_t, DWORD, DWORD); static void *fixture_fail_native(const char *, size_t); static void *fixture_fail_body(void *);\n",
					"static int fixture_fail(const char *);\n#define COUNTRY_PARTS_ALLOC(n) fixture_fail_heap(\"country-parts\",HEAP_ZERO_MEMORY,(n))\n#define COUNTRY_POOL_CREATE() (fixture_fail(\"country-pool\")?NULL:CreateThreadpool(NULL))\n#define COUNTRY_WORK_CREATE(f,p,e) (fixture_fail(\"country-work\")?NULL:CreateThreadpoolWork((f),(p),(e)))\n",
					"#define DAILY_A_HEAP(k,f,n) fixture_fail_heap(k,f,n)\n#define DAILY_A_VIRTUAL(k,a,n,f,p) fixture_fail_virtual(k,a,n,f,p)\n#define DAILY_A_NATIVE(k,n) fixture_fail_native(k,n)\n#define DAILY_A_BODY(s) fixture_fail_body(s)\n",
					"#define FOCH_MONTHLY_FIXTURE_PROFILE member_monthly_profile\nstatic const unsigned char member_monthly_profile[]={{0x49,0x8B,0x87,0xE8,0x1C,0x00,0x00,0x45,0x0F,0xB6,0xC4,0x41,0x0F,0xB6,0xD5,0x48,0x8B,0x0C,0xD8,0xE8,0x00,0x00,0x00,0x00,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0xFF,0xC7,0x48,0xFF,0xC3,0x48,0x3B,0xDE,0x7C,0xB7}};\n#define FOCH_ACTIVE_FIXTURE_HEADER member_active_header\nstatic const unsigned char member_active_header[]={{0x44,0x8B,0xE7,0x48,0x8B,0x8D,0xE8,0x1C,0x00,0x00,0x48,0x8B,0x85,0xF0,0x1C,0x00,0x00,0x48,0x2B,0xC1,0x48,0xC1,0xF8,0x03,0x85,0xC0,0x0F,0x8E,0xCE,0x01,0x00,0x00,0x0F,0x1F,0x44,0x00,0x00}};\n",
					"#define FOCH_ACTIVE_FIXTURE_PROFILE member_fixture_profile\nstatic const unsigned char member_fixture_profile[]={{0x41,0x8B,0xC4,0x48,0x8D,0x34,0xC5,0x00,0x00,0x00,0x00,0x48,0x89,0x74,0x24,0x60,0x4C,0x8D,0x04,0x0E,0x4D,0x8B,0x30,0x49,0x8B,0x5E,0x20,0x48,0x8B,0xCB,0x48,0xC1,0xE9,0x30,0x48,0x8B,0xC3,0x48,0xC1,0xE8,0x30,0x3C,0x07,0x0F,0x84,0xCF,0x00,0x00,0x00,0x41,0x83,0xBE,0x44,0x13,0x00,0x00,0x00,0x75,0x35,0x80,0xF9,0x07,0x74,0x30,0x48,0x8B,0xC3,0x48,0xC1,0xE8,0x20,0x0F,0xBF,0xC8,0x3B,0x0D,0x00,0x00,0x00,0x00,0x74,0x1E,0x3B,0x0D,0x00,0x00,0x00,0x00,0x74,0x16,0x3B,0x0D,0x00,0x00,0x00,0x00,0x74,0x0E,0x41,0x83,0xBE,0x28,0x0D,0x00,0x00,0x00,0x0F,0x84,0x90,0x00,0x00,0x00,0x41,0x8B,0x45,0x14,0x49,0x8B,0x4D,0x08,0x48,0x89,0x1C,0xC1,0xFF,0xC0,0x41,0x89,0x45,0x14,0xFF,0x85,0x14,0x1D,0x00,0x00,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x80,0x7C,0x24,0x68,0x00,0x0F,0x84,0x9D,0x00,0x00,0x00,0x41,0x83,0xBE,0x44,0x13,0x00,0x00,0x00,0x75,0x0B,0x41,0x80,0x7E,0x26,0x07,0x0F,0x85,0x88,0x00,0x00,0x00,0x49,0x8B,0xCE,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0xE8,0x00,0x00,0x00,0x00,0x85,0xC0,0x0F,0x84,0x69,0x00,0x00,0x00,0x48,0x8B,0x85,0x38,0x1D,0x00,0x00,0x4C,0x89,0x30,0x48,0x83,0x85,0x38,0x1D,0x00,0x00,0x08,0x81,0xC7,0xE8,0x03,0x00,0x00,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x90,0x41,0xFF,0xC4,0x48,0x8B,0x8D,0xE8,0x1C,0x00,0x00,0x48,0x8B,0x85,0xF0,0x1C,0x00,0x00,0x48,0x2B,0xC1,0x48,0xC1,0xF8,0x03,0x44,0x3B,0xE0,0x0F,0x8C,0x37,0xFE,0xFF,0xFF}};\n#define foch_plugin_get_api fixture_original_get_api\n",
					"#include \"{}\"\n",
					"#undef foch_plugin_get_api\n",
					"static const char *fixture_fail_kind; static unsigned fixture_fail_seen, fixture_fail_nth; static int fixture_fail(const char *kind) {{ if(!fixture_fail_kind || strcmp(kind,fixture_fail_kind)) return 0; return ++fixture_fail_seen == fixture_fail_nth; }}\n",
					"static void *fixture_fail_heap(const char *k,DWORD f,size_t n) {{ return fixture_fail(k)?NULL:HeapAlloc(GetProcessHeap(),f,n); }}\n",
					"static void *fixture_fail_virtual(const char *k,void *a,size_t n,DWORD f,DWORD p) {{ return fixture_fail(k)?NULL:VirtualAlloc(a,n,f,p); }}\n",
					"static void *fixture_fail_native(const char *k,size_t n) {{ return fixture_fail(k)?NULL:daily_a_call_alloc(n); }}\n",
					"static void *fixture_fail_body(void *s) {{ return fixture_fail(\"body\")?NULL:alloc_near(s,DAILY_A_STUBS_AT+0xA0); }}\n",
					"__declspec(dllexport) void fixture_daily_fault(const char *kind, unsigned nth) {{ fixture_fail_kind=kind; fixture_fail_nth=nth; fixture_fail_seen=0; }}\n",
					"__declspec(dllexport) unsigned fixture_daily_fault_hits(void) {{ return fixture_fail_seen; }}\n",
					"static void *fixture_range_site; static int32_t fixture_range_find(const FochHostApi *h, FochStr pattern, void **address) {{ (void)h; (void)pattern; *address=fixture_range_site; return FOCH_OK; }}\n",
					"__declspec(dllexport) int fixture_daily_range(void *base, size_t bytes, void *site) {{ FochHostApi h={{0}}; int found; void *result; h.exe_base=base; h.exe_size=bytes; h.find_pattern=fixture_range_find; fixture_range_site=site; return install_daily_a(&h,&found,&result); }}\n",
					"__declspec(dllexport) void *fixture_daily_build(void) {{ return build_daily_a_body(daily_a_site, 0); }}\n",
					"__declspec(dllexport) void *fixture_daily_build_at(void *site) {{ void *old=daily_a_image; size_t bytes=daily_a_image_size; void *result; daily_a_image=(void*)0x10000000; daily_a_image_size=0x40000; result=build_daily_a_body(site,0); daily_a_image=old; daily_a_image_size=bytes; return result; }}\n",
					"__declspec(dllexport) unsigned fixture_daily_owner_count(void) {{ return daily_a_arena_count; }}\n",
					"__declspec(dllexport) unsigned fixture_daily_arena(unsigned mode) {{\n",
					"unsigned i, result = 0; struct daily_a_arena *a = NULL; void *p, *first;\n",
					"if (mode == 0) {{ for (i=0;i<33;++i) {{ a=daily_a_arena_new(); if (!a) continue; ++result; da_tls_arena=a; p=daily_a_route_alloc(32); if(p) daily_a_route_free(p,32); }} }}\n",
					"else {{ a=daily_a_arena_new(); if (!a) return 999; da_tls_arena=a; if(mode==1) {{ first=daily_a_route_alloc(32); p=first; for(i=0;i<3;++i) {{ if(p==first) ++result; daily_a_route_free(p,32); p=daily_a_route_alloc(32); }} daily_a_route_free(p,32); }}\n",
					"else {{ const size_t sizes[]={{0x3D0,0x3D1,0x3E0,0x3E1,0x3F0,0x3F1}}; for(i=0;i<6;++i) {{ p=daily_a_route_alloc(sizes[i]); if(!p) return 998; daily_a_route_free(p,sizes[i]); ++result; }} }} }} da_tls_arena=NULL; return result; }}\n",
					"__declspec(dllexport) int fixture_daily_replay_targets(void *shift, void *publish) {{ return daily_a_real_shift == shift && daily_a_real_publish == publish; }}\n",
					"__declspec(dllexport) unsigned fixture_daily_slice(void *game, void **snapshot, unsigned char *shadow, unsigned char *copy, unsigned long long from, unsigned long long to, void *ops) {{\n",
					"struct daily_a_part part = {{0}}; part.game = game; part.snapshot = snapshot; part.shadow = shadow; part.copy = copy; part.from = from; part.to = to; part.op_cap=(unsigned)(2*(to-from)); part.ops=HeapAlloc(GetProcessHeap(),HEAP_ZERO_MEMORY,part.op_cap*sizeof(*part.ops)); if(!part.ops) return 997; daily_a_run_part(&part);\n",
					"unsigned count = part.op_count; if (count <= 64) memcpy(ops, part.ops, count * sizeof(*part.ops)); if (part.ops) HeapFree(GetProcessHeap(), 0, part.ops); return count; }}\n"
				),
				plugin.join("country_month.c").display().to_string().replace('\\', "/")
			),
		)
		.unwrap();
		let mut member_source = fs::read_to_string(&builder_probe).unwrap();
		member_source.push_str(r#"
static void *member_month, *member_state, *member_active;
static unsigned member_hooks;
static int32_t member_find(const FochHostApi *h,FochStr pattern,void **address) {
    (void)h;
    *address=NULL;
    if(pattern.len>=8 && !memcmp(pattern.ptr,"49 8B 87",8)) *address=member_month;
    else if(pattern.len>=8 && !memcmp(pattern.ptr,"40 53 55",8)) *address=member_state;
    else if(pattern.len>=8 && !memcmp(pattern.ptr,"44 8B E7",8)) *address=member_active;
    return *address ? FOCH_OK : FOCH_E_NOT_FOUND;
}
struct member_patch { unsigned char *site,*copy; void *detour,*next; size_t len; unsigned char original[17]; };
static struct member_patch member_patch_hooks[3];
static int member_patch_mode;
static void member_jump(unsigned char *at,void *target) {memcpy(at,"\xFF\x25\0\0\0\0",6);memcpy(at+6,&target,8);}
static int32_t member_create(const FochHostApi *h,void *site,void *detour,int32_t priority,void *const **next,FochHook **hook) {
    (void)h;(void)site;(void)detour;(void)priority;
    if(member_patch_mode) {
        struct member_patch *p=&member_patch_hooks[member_hooks++];p->site=site;p->detour=detour;
        p->len=detour==(void *)country_detour?15:17;
        memcpy(p->original,site,p->len);p->copy=VirtualAlloc(NULL,64,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READWRITE);
        if(!p->copy)return FOCH_E_MEMORY;memcpy(p->copy,site,p->len);member_jump(p->copy+p->len,(unsigned char *)site+p->len);
        if(!FlushInstructionCache(GetCurrentProcess(),p->copy,64))return FOCH_E_MEMORY;
        p->next=p->copy;*next=(void *const *)&p->next;*hook=(FochHook *)p;
    } else {*next=NULL; *hook=(FochHook *)(uintptr_t)(++member_hooks);}
    return FOCH_OK;
}
static int32_t member_enable(const FochHostApi *h,FochHook *hook,int32_t enabled) {
    (void)h;
    if(member_patch_mode) {struct member_patch *p=(struct member_patch *)hook;
        if(enabled) {member_jump(p->site,p->detour);memset(p->site+14,0x90,p->len-14);} else memcpy(p->site,p->original,p->len);
        if(!FlushInstructionCache(GetCurrentProcess(),p->site,p->len))return FOCH_E_MEMORY;
    }
    return FOCH_OK;
}
static void member_report(const FochHostApi *h,const FochStatus *s) {(void)h;(void)s;}
static void member_host(FochHostApi *h,void *base,size_t bytes) {
    memset(h,0,sizeof(*h)); h->size=sizeof(*h);h->abi_major=FOCH_ABI_MAJOR;
    h->exe_base=base;h->exe_size=bytes;h->find_pattern=member_find;
    h->hook_create=member_create;h->hook_set_enabled=member_enable;h->report=member_report;
    member_hooks=0;
}
__declspec(dllexport) int fixture_member_edges(unsigned kind) {
    SYSTEM_INFO info; FochHostApi h; MEMORY_BASIC_INFORMATION region;
    unsigned char *image,*site,*fn; DWORD old, protection;
    unsigned round,refused=0,preserved=0,expected=kind==2?4:2;
    int32_t rel,rc; char text[160];
    static const unsigned char month[]={0x49,0x8B,0x87,0xE8,0x1C,0,0,0x45,0x0F,0xB6,0xC4,0x41,0x0F,0xB6,0xD5};
    static const unsigned char active[]={0x44,0x8B,0xE7,0x48,0x8B,0x8D,0xE8,0x1C,0,0,0x48,0x8B,0x85,0xF0,0x1C,0,0};
    GetSystemInfo(&info);
    image=VirtualAlloc((void*)0x10000000,3*info.dwPageSize,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READWRITE);
    if(!image) return 110;
    for(round=0;round<expected;++round) {
        VirtualProtect(image+info.dwPageSize,info.dwPageSize,PAGE_EXECUTE_READWRITE,&old);
        memset(image,0,3*info.dwPageSize);
        member_month=kind==0?image+info.dwPageSize-sizeof(month):image+0x100;
        member_state=NULL;member_active=NULL;
        memcpy(member_month,month,sizeof(month));
        if(kind!=0) {
            unsigned char *m=member_month;
            memcpy(m,FOCH_MONTHLY_FIXTURE_PROFILE,sizeof(FOCH_MONTHLY_FIXTURE_PROFILE));
            m[0x13]=0xE8;rel=(int32_t)((image+0x500)-(m+0x18));memcpy(m+0x14,&rel,4);
            image[0x500]=0xC3;
            member_state=image+0x200;
            memcpy(member_state,"\x40\x53\x55\x56\x57\x41\x54\x41\x55\x41\x56\x41\x57\x48\x83\xEC\x28\x48\x8B\xF9\x45\x33\xED",23);
            site=kind==1?image+info.dwPageSize-sizeof(active):image+0x800;
            member_active=site;memcpy(site,active,sizeof(active));
            if(kind==2) {
                unsigned cmp;
                memcpy(site,FOCH_ACTIVE_FIXTURE_HEADER,sizeof(FOCH_ACTIVE_FIXTURE_HEADER));
                memcpy(site+0x25,FOCH_ACTIVE_FIXTURE_PROFILE,sizeof(FOCH_ACTIVE_FIXTURE_PROFILE));
                fn=image+0x580;memcpy(fn-4,"\x0D\xF0\xDE\xC0",4);*fn=0xC3;
                for(cmp=0;cmp<3;++cmp) {
                    size_t at=0x6F+cmp*8;
                    rel=(int32_t)((image+0x600+cmp*4)-(site+at+6));memcpy(site+at+2,&rel,4);
                }
                if(round<2) {rel=(int32_t)((image+info.dwPageSize+16)-(site+0x75));memcpy(site+0x71,&rel,4);}
                if(round==2) {rel=INT32_MIN;memcpy(site+0x71,&rel,4);}
                if(round==3) fn=image+2*info.dwPageSize;
                rel=(int32_t)(fn-(site+0x15C));memcpy(site+0x158,&rel,4);
            }
        }
        protection=(round==1?PAGE_EXECUTE_READ|PAGE_GUARD:PAGE_NOACCESS);
        if(!VirtualProtect(image+info.dwPageSize,info.dwPageSize,protection,&old)) return 111;
        member_host(&h,image,kind==2?3*info.dwPageSize:info.dwPageSize);
        rc=init(&h);
        if(rc!=FOCH_OK) ++refused;
        if(!VirtualQuery(image+info.dwPageSize,&region,sizeof(region))) return 112;
        if(region.Protect==protection) ++preserved;
    }
    snprintf(text,sizeof(text),"refused=%u\npreserved=%u\nexpected=%u\n",refused,preserved,expected);
    write_proof("member-edges.txt",text);
    VirtualFree(image,0,MEM_RELEASE);return 0;
}
__declspec(dllexport) int fixture_active_extra_call(void) {
    unsigned char *image,*site,*copy,*extra;unsigned i;int32_t rel;int found,created,enabled,rc;
    void *address;uintptr_t moved=0;FochHostApi h;char text[240];
    image=VirtualAlloc((void*)0x10000000,0x40000,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READWRITE);
    if(!image) return 113;
    site=image+0x10000;
    memcpy(site,active_header_image,sizeof(active_header_image));
    memcpy(site+ACTIVE_BODY_LO,active_body_image,sizeof(active_body_image));
    image[0xE000]=0xC3;
    for(i=0;i<8;++i) point_disp(site+active_call_at[i],1,5,image+0xE000);
    for(i=0;i<3;++i) point_disp(site+0x6F+i*8,2,6,image+0x8000+i*4);
    copy=build_active_body(site,(void*)active_invoke_back);
    i=copy!=NULL;if(copy) VirtualFree(copy,0,MEM_RELEASE);
    site[0xFB]=0xE8;rel=(int32_t)((image+0xE000)-(site+0x100));memcpy(site+0xFC,&rel,4);memset(site+0x100,0x90,5);
    extra=build_active_body(site,(void*)active_invoke_back);
    if(extra) {memcpy(&rel,extra+(0xFC-ACTIVE_BODY_LO),4);moved=(uintptr_t)(extra+(0x100-ACTIVE_BODY_LO)+rel);VirtualFree(extra,0,MEM_RELEASE);}
    member_month=member_state=NULL;member_active=site;member_host(&h,image,0x40000);
    rc=install_active(&h,&found,&created,&enabled,&address);
    if(active_body) {VirtualFree(active_body,0,MEM_RELEASE);active_body=NULL;}
    /* Exercise the actual original added CALL plus five NOPs and a return. */
    site[0x105]=0xC3;FlushInstructionCache(GetCurrentProcess(),site+0xFB,11);
    ((void(*)(void))(site+0xFB))();
    snprintf(text,sizeof(text),"known-supported=%u\nextra-refused=%d\ninit-refused=%d\noriginal-returned=1\nmoved-target=%llx\n",i,!extra,rc!=FOCH_OK,(unsigned long long)moved);
    write_proof("active-profile.txt",text);VirtualFree(image,0,MEM_RELEASE);return 0;
}

/* Known production bytes exercise the real init/profile/builder in a private
 * synthetic PE-sized allocation; no installed code is ever executed. */
__declspec(dllexport) int fixture_known_production_profiles(void) {
    unsigned char *image=VirtualAlloc(NULL,0x25E0000,MEM_COMMIT|MEM_RESERVE,PAGE_EXECUTE_READWRITE);
    FochHostApi h;FochStatus st;int32_t rc;char text[256];int copy;
    if(!image)return 121;
    member_month=image+0x75DD30;member_active=image+0x75AC2B;member_state=image+0x100;
    memcpy(member_month,monthly_image,sizeof(monthly_image));
    memcpy(member_active,active_header_image,sizeof(active_header_image));
    memcpy((unsigned char *)member_active+0x25,active_body_image,sizeof(active_body_image));
    memcpy(member_state,"@SUVWATAUAVAWHì(HùE3í",23);
    member_host(&h,image,0x25E0000);rc=init(&h);status(&st);copy=active_body!=NULL;
    snprintf(text,sizeof(text),"init-rc=%d\nstate=%d\ncreated=%u\nactive-real=%d\ncopy-built=%d\nmonthly-profile=%d\nactive-header=%d\n",rc,st.state,member_hooks,active_real,copy,monthly_profile_is_known(&h,member_month),active_header_is_known(member_active,0));
    write_proof("known-profiles.txt",text);
    if(active_body&&!VirtualFree(active_body,0,MEM_RELEASE))return 122;active_body=NULL;
    return VirtualFree(image,0,MEM_RELEASE)?0:123;
}

__declspec(dllexport) int fixture_monthly_image(void *base,void *primary,void *helper,void *state_body,void *join) {
    unsigned char *image=base,*state=image+0x1000;size_t at=23;unsigned char *site=image+0x75DD30;
    memcpy(site,monthly_image,sizeof(monthly_image));member_jump(site+0x49,join);
    member_jump(image+0x2F36C0,primary);
    memcpy(image+0x769EA0,"\xFF\x81\0\x01\0\0",6);member_jump(image+0x769EA0+6,helper);
    memcpy(state,"\x40\x53\x55\x56\x57\x41\x54\x41\x55\x41\x56\x41\x57\x48\x83\xEC\x28\x48\x8B\xF9\x45\x33\xED",23);
    memcpy(state+at,"\x48\x83\xEC\x20\x48\x8B\xCF\x48\xB8",9);at+=9;memcpy(state+at,&state_body,8);at+=8;
    memcpy(state+at,"\xFF\xD0\x48\x83\xC4\x20\x48\x83\xC4\x28\x41\x5F\x41\x5E\x41\x5D\x41\x5C\x5F\x5E\x5D\x5B\xC3",23);
    return FlushInstructionCache(GetCurrentProcess(),image,0x25E0000)?0:1;
}
__declspec(dllexport) int fixture_monthly_install(void *image) {
    FochHostApi h;member_patch_mode=1;member_month=(unsigned char *)image+0x75DD30;member_state=(unsigned char *)image+0x1000;member_active=NULL;
    member_host(&h,image,0x25E0000);return init(&h);
}

/* Exercise the real host's hook table through a fault-injecting SDK proxy. */
struct init_hook_probe { FochHook *hook; void *target; void *const **next; unsigned char bytes[14]; const char *phase; int enabled; };
static struct init_hook_probe init_hooks[8];
static unsigned init_hook_count, init_rollback_errors, init_disable_count, init_enable_attempts;
static const FochHostApi *init_real_host;
static FochHostApi init_proxy_host;
static FochPluginApi init_proxy_api;
static char init_fault[80], init_disable_order[128];
static void *init_magic_address;
static uint32_t init_magic_value;
static const char *init_phase(void *detour) {
    if(detour==(void *)state_detour)return "state";
    if(detour==(void *)country_detour)return "country";
    if(detour==(void *)active_detour)return "active";
    if(detour==(void *)daily_a_alloc_detour)return "alloc";
    if(detour==(void *)daily_a_free_detour)return "free";
    if(detour==(void *)daily_a_end_detour)return "end";
    return "daily";
}
static int init_fault_is(const char *phase,const char *action) {
    char text[80];snprintf(text,sizeof(text),"%s-%s",phase,action);return !strcmp(text,init_fault);
}
static int32_t init_find(const FochHostApi *h,FochStr pattern,void **out) {
    int32_t rc; (void)h;rc=init_real_host->find_pattern(init_real_host,pattern,out);
    if(rc==FOCH_OK&&((!strcmp(init_fault,"monthly-gap")&&pattern.len>8&&!memcmp(pattern.ptr,"49 8B 87",8))||
        (!strcmp(init_fault,"active-gap")&&pattern.len>8&&!memcmp(pattern.ptr,"44 8B E7",8)))) {
        static const unsigned char month[]={0x41,0xFF,0x87,0x00,0x01,0x00,0x00};
        static const unsigned char active[]={0xFF,0x45,0x7C,0x90,0x90};
        int monthly=!strcmp(init_fault,"monthly-gap");unsigned char *at=(unsigned char *)*out+(monthly?0x18:0x20);
        size_t bytes=monthly?sizeof(month):sizeof(active);DWORD old;
        if(!VirtualProtect(at,bytes,PAGE_EXECUTE_READWRITE,&old))return FOCH_E_MEMORY;
        memcpy(at,monthly?month:active,bytes);
        if(!VirtualProtect(at,bytes,old,&old)||!FlushInstructionCache(GetCurrentProcess(),at,bytes))return FOCH_E_MEMORY;
    }
    if(rc==FOCH_OK&&!strcmp(init_fault,"active-shape")&&pattern.len>8&&!memcmp(pattern.ptr,"44 8B E7",8)) {
        int32_t rel;DWORD old;unsigned char *code=*out;memcpy(&rel,code+0x158,4);
        init_magic_address=code+0x15C+rel-4;memcpy(&init_magic_value,init_magic_address,4);
        if(!VirtualProtect(init_magic_address,4,PAGE_EXECUTE_READWRITE,&old))return FOCH_E_MEMORY;
        memset(init_magic_address,0,4);VirtualProtect(init_magic_address,4,old,&old);
    }
    return rc;
}
static void init_report(const FochHostApi *h,const FochStatus *st) {(void)h;init_real_host->report(init_real_host,st);}
static int32_t init_create(const FochHostApi *h,void *target,void *detour,int32_t priority,void *const **next,FochHook **hook) {
    const char *phase=init_phase(detour);int32_t rc;unsigned char bytes[14];(void)h;
    if(init_fault_is(phase,"create"))return FOCH_E_MEMORY;
    memcpy(bytes,target,sizeof(bytes));rc=init_real_host->hook_create(init_real_host,target,detour,priority,next,hook);
    if(rc==FOCH_OK) {
        struct init_hook_probe *p=&init_hooks[init_hook_count++];p->hook=*hook;p->target=target;p->next=next;p->phase=phase;memcpy(p->bytes,bytes,sizeof(bytes));
    }
    return rc;
}
static int32_t init_enable(const FochHostApi *h,FochHook *hook,int32_t enable) {
    unsigned i;int32_t rc;(void)h;
    for(i=0;i<init_hook_count;i++)if(init_hooks[i].hook==hook) {
        struct init_hook_probe *p=&init_hooks[i];
        if(enable) init_enable_attempts++;
        if(enable&&(init_fault_is(p->phase,"enable")||(!strcmp(init_fault,"active-enable-rollback")&&!strcmp(p->phase,"active"))))return FOCH_E_CONFLICT;
        if(!enable) {
            init_disable_count++;
            snprintf(init_disable_order+strlen(init_disable_order),sizeof(init_disable_order)-strlen(init_disable_order),"%s%s",init_disable_order[0]?",":"",p->phase);
            if(!strcmp(init_fault,"active-enable-rollback")&&!strcmp(p->phase,"country")) {init_rollback_errors++;return FOCH_E_MEMORY;}
        }
        rc=init_real_host->hook_set_enabled(init_real_host,hook,enable);
        if(rc==FOCH_OK)p->enabled=enable!=0;
        if(enable&&rc==FOCH_OK&&init_fault_is(p->phase,"enable-after"))return FOCH_E_CONFLICT;
        return rc;
    }
    return FOCH_E_INVALID;
}
static int32_t init_with_fault(const FochHostApi *h) {
    unsigned i,enabled=0,unchanged=0,original=0,daily=0;int32_t rc;FochStatus st;char text[1024];
    init_real_host=h;init_proxy_host=*h;init_proxy_host.find_pattern=init_find;init_proxy_host.report=init_report;
    init_proxy_host.hook_create=init_create;init_proxy_host.hook_set_enabled=init_enable;
    rc=fixture_original_get_api(FOCH_ABI_MAJOR)->init(&init_proxy_host);
    fixture_original_get_api(FOCH_ABI_MAJOR)->status(&st);
    if(init_magic_address) {DWORD old;VirtualProtect(init_magic_address,4,PAGE_EXECUTE_READWRITE,&old);memcpy(init_magic_address,&init_magic_value,4);VirtualProtect(init_magic_address,4,old,&old);}
    for(i=0;i<init_hook_count;i++) {
        struct init_hook_probe *p=&init_hooks[i];int same=!memcmp(p->target,p->bytes,sizeof(p->bytes));
        enabled+=p->enabled;unchanged+=same;
        original+=same||(*p->next&&**p->next&&!memcmp(**p->next,p->bytes,sizeof(p->bytes)));
        daily+=!strcmp(p->phase,"daily");
    }
    snprintf(text,sizeof(text),"init-rc=%d\nplugin-status=%d\nreason=%d\ndetail=%.*s\ncreated=%u\nenabled=%u\nraw-unchanged=%u\noriginal-prefix=%u\ndaily-created=%u\nrollback-errors=%u\nenable-attempts=%u\ndisable-count=%u\ndisable-order=%s\n",rc,st.state,st.reason_code,(int)st.detail.len,st.detail.ptr,init_hook_count,enabled,unchanged,original,daily,init_rollback_errors,init_enable_attempts,init_disable_count,init_disable_order);
    write_proof("init-fault.txt",text);return rc;
}
__declspec(dllexport) const FochPluginApi *foch_plugin_get_api(uint32_t major) {
    const FochPluginApi *original=fixture_original_get_api(major);
    if(!GetEnvironmentVariableA("FOCH_COUNTRY_INIT_FAULT",init_fault,sizeof(init_fault)))return original;
    init_proxy_api=*original;init_proxy_api.init=init_with_fault;return &init_proxy_api;
}

"#);
		fs::write(&builder_probe, member_source).unwrap();
		let countries_asm = ml
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/c", "/Focountry_detour.obj"])
			.arg(plugin.join("country_detour.asm"))
			.output()
			.unwrap();
		assert!(
			countries_asm.status.success(),
			"countries asm: {} {}",
			String::from_utf8_lossy(&countries_asm.stdout),
			String::from_utf8_lossy(&countries_asm.stderr)
		);
		let countries_dll = tool
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/LD", "/W4", "/utf-8"])
			.arg(format!(
				"/I{}",
				package.join("../foch-plugin-sdk/include").display()
			))
			.arg(&builder_probe)
			.args([
				"/Focountry-c.obj",
				"/Feeu4_country_month.dll",
				"country_detour.obj",
			])
			// Export internal ASM cells only in this disposable fixture DLL.
			.args([
				"/link",
				"/EXPORT:daily_a_detour",
				"/EXPORT:daily_a_next,DATA",
				"/EXPORT:daily_a_running,DATA",
				"/EXPORT:daily_a_ready,DATA",
				"/EXPORT:states_done,DATA",
				"/EXPORT:daily_a_worker",
				"/EXPORT:daily_a_worker_ret",
				"/EXPORT:daily_a_note_shift",
			])
			.output()
			.unwrap();
		assert!(
			countries_dll.status.success(),
			"countries: {} {}",
			String::from_utf8_lossy(&countries_dll.stdout),
			String::from_utf8_lossy(&countries_dll.stderr)
		);
		let output = tool
			.to_command()
			.current_dir(root.path())
			.args(["/nologo", "/W4", "/utf-8"])
			.arg(package.join("tests/fixtures/game.c"))
			.args(["/FoGame.obj", "/Feeu4.exe", "sites.obj", "version.lib"])
			.output()
			.unwrap();
		assert!(
			output.status.success(),
			"game: {} {}",
			String::from_utf8_lossy(&output.stdout),
			String::from_utf8_lossy(&output.stderr)
		);
		root
	})
}

struct Runtime(TempDir);
impl Runtime {
	fn new() -> Self {
		let temp = tempfile::Builder::new()
			.prefix("foch-加载-")
			.tempdir()
			.unwrap();
		fs::create_dir(temp.path().join("foch-host")).unwrap();
		fs::write(temp.path().join("foch-runtime"), "1\n").unwrap();
		fs::create_dir(temp.path().join("plugins")).unwrap();
		fs::copy(
			fixtures().path().join("eu4.exe"),
			temp.path().join("eu4.exe"),
		)
		.unwrap();
		let dll = std::env::current_exe()
			.unwrap()
			.parent()
			.unwrap()
			.join("foch_runtime_host.dll");
		assert!(dll.is_file(), "host artifact missing: {}", dll.display());
		foch_runtime::host::validate_host(&fs::read(&dll).unwrap()).unwrap();
		fs::copy(dll, temp.path().join("VERSION.dll")).unwrap();
		fs::copy(
			fixtures().path().join("native.dll"),
			temp.path().join("plugins/unexpected.dll"),
		)
		.unwrap();
		Self(temp)
	}
	fn path(&self, path: &str) -> PathBuf {
		self.0.path().join(path)
	}
	fn plugin(&self, name: &str, kind: &str, phase: &str) -> Value {
		let path = self.path(&format!("plugins/{name}.dll"));
		fs::copy(fixtures().path().join(format!("{name}.dll")), &path).unwrap();
		json!({"id":name, "version":"0.1.0", "kind":kind, "phase":phase,
			"path":path.to_str().unwrap(), "config_json":"{\"greeting\":\"hi\"}",
			"abi_major":if kind == "native" {Some(0)} else {None}})
	}
	fn plan(&self, plugins: Vec<Value>) {
		fs::write(
			self.path("foch-host/plan.json"),
			serde_json::to_vec_pretty(&json!({
				"format":1, "run_id":"fixture-run", "game_version":"1.37.5.0",
				"events":self.path("foch-host/events.jsonl"), "plugins":plugins
			}))
			.unwrap(),
		)
		.unwrap();
	}
	fn run(&self, mode: &str) -> Vec<Value> {
		run_runtime(self.0.path(), mode)
	}
}

fn run_runtime(directory: &Path, mode: &str) -> Vec<Value> {
	for marker in [
		"game-constructor-entered",
		"game-main-entered",
		"game-version-resolved",
		"fixture-complete",
	] {
		let path = directory.join("foch-host").join(marker);
		if path.exists() {
			fs::remove_file(path).unwrap();
		}
	}
	let expected: Vec<String> = fs::read(directory.join("foch-host/plan.json"))
		.ok()
		.and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
		.and_then(|plan| plan["plugins"].as_array().cloned())
		.unwrap_or_default()
		.iter()
		.filter_map(|plugin| plugin["id"].as_str().map(str::to_owned))
		.collect();
	let mut child = Command::new(directory.join("eu4.exe"))
		.arg(mode)
		.env_remove("FOCH_DAILY_A")
		.env(
			"FOCH_COUNTRY_SERIAL",
			if mode == "monthly-callback-serial" {
				"1"
			} else {
				"0"
			},
		)
		.env(
			"FOCH_COUNTRY_INIT_FAULT",
			mode.strip_prefix("init-fault-")
				.or_else(|| mode.strip_prefix("profile-"))
				.unwrap_or(""),
		)
		.env(
			"FOCH_FIXTURE_DELAY_MS",
			if mode == "delayed" { "1200" } else { "40" },
		)
		.current_dir(directory)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	// Include process/DLL/CRT startup in addition to the C fixture's separate
	// 10-second event acknowledgement (or concurrent-forwarding) deadline.
	let deadline = Instant::now() + Duration::from_secs(30);
	while child.try_wait().unwrap().is_none() {
		let events = read_events(directory);
		let callbacks_complete = expected.iter().all(|id| {
			!matches!(
				id.as_str(),
				"async_native"
					| "unaligned_report"
					| "unreadable_report"
					| "invalid_report_detail"
					| "short_report"
			) || directory
				.join(format!("foch-host/callback-finished-{id}"))
				.exists()
		});
		if callbacks_complete
			&& expected.iter().all(|id| {
				states(&events, id).last().is_some_and(|state| {
					matches!(
						*state,
						"active" | "inactive" | "refused" | "failed" | "unknown"
					)
				})
			}) {
			fs::write(directory.join("foch-host/fixture-complete"), "1").unwrap();
		}
		if Instant::now() > deadline {
			let stages: Vec<_> = [
				"game-constructor-entered",
				"game-main-entered",
				"game-version-resolved",
				"fixture-complete",
			]
			.into_iter()
			.map(|name| (name, directory.join("foch-host").join(name).exists()))
			.collect();
			let diagnostic = fs::read_to_string(directory.join("foch-host/host-error.txt"));
			let _ = child.kill();
			let output = child.wait_with_output().unwrap();
			panic!(
				"loader timed out: mode={mode}, stages={stages:?}, callbacks_complete={callbacks_complete}, events={events:?}, diagnostic={diagnostic:?}, notes={}; child={:?}, stdout={}, stderr={}",
				host_notes(directory),
				output.status.code(),
				String::from_utf8_lossy(&output.stdout),
				String::from_utf8_lossy(&output.stderr)
			);
		}
		std::thread::sleep(Duration::from_millis(20));
	}
	let output = child.wait_with_output().unwrap();
	assert!(
		output.status.success(),
		"fixture failed: {:?}; {} {}; diagnostic={:?}; notes={}",
		output.status.code(),
		String::from_utf8_lossy(&output.stdout),
		String::from_utf8_lossy(&output.stderr),
		fs::read_to_string(directory.join("foch-host/host-error.txt")),
		host_notes(directory)
	);
	read_events(directory)
}

fn host_notes(directory: &Path) -> String {
	let mut notes = String::new();
	let Ok(entries) = fs::read_dir(directory.join("foch-host")) else {
		return notes;
	};
	for entry in entries.flatten() {
		let path = entry.path();
		if path.extension().and_then(|ext| ext.to_str()) != Some("txt") {
			continue;
		}
		let name = path
			.file_name()
			.and_then(|name| name.to_str())
			.unwrap_or("?");
		notes.push_str(name);
		notes.push('\n');
		notes.push_str(&fs::read_to_string(&path).unwrap_or_default());
		notes.push('\n');
	}
	notes
}

fn read_events(directory: &Path) -> Vec<Value> {
	fs::read_to_string(directory.join("foch-host/events.jsonl"))
		.unwrap_or_default()
		.split_inclusive('\n')
		.filter(|line| line.ends_with('\n'))
		.map(|line| serde_json::from_str(line).unwrap())
		.collect()
}

fn states<'a>(events: &'a [Value], id: &str) -> Vec<&'a str> {
	events
		.iter()
		.filter(|v| v["plugin_id"] == id && v["event"] == "state")
		.filter_map(|v| v["state"].as_str())
		.collect()
}

#[test]
fn published_hooks_resolve_chain_and_redirect_in_the_fixture_process() {
	let rt = Runtime::new();
	rt.plan(vec![
		rt.plugin("hooks", "native", "entry"),
		rt.plugin("hooks_late", "native", "deferred"),
	]);
	let events = rt.run("hooks");
	assert_eq!(states(&events, "hooks").last(), Some(&"active"));
	assert_eq!(states(&events, "hooks_late").last(), Some(&"active"));
	let entry = fs::read_to_string(rt.path("foch-host/hook-entry.txt")).unwrap();
	let main = fs::read_to_string(rt.path("foch-host/hook-main.txt")).unwrap();
	let late = fs::read_to_string(rt.path("foch-host/hook-late.txt")).unwrap();
	let entry = proof(&entry);
	let main = proof(&main);
	let late = proof(&late);
	assert_eq!(entry["unique"], entry["expected"], "{entry:?}");
	assert_ne!(entry["unique"], "0");
	assert_eq!(entry["missing"], "-5");
	assert_eq!(entry["ambiguous"], "-6");
	assert_eq!(entry["lookup-bytes"], "unchanged");
	assert_eq!(entry["chain"], "high,low,orig");
	assert_eq!(entry["chain-after-disable"], "high,orig");
	assert_eq!(entry["chain-bytes"], "changed");
	assert_eq!(entry["unfit"], "-2");
	assert_eq!(entry["unfit-bytes"], "unchanged");
	assert_eq!(main["redirect-call"], "high,orig");
	assert_eq!(main["redirect-bytes"], "ff25");
	assert_eq!(main["redirect-after-disable"], "ff25");
	assert_eq!(main["redirect-value"], "7");
	assert_eq!(main["prologue-after-call"], "ff25");
	let workers: i32 = main["worker-count"].parse().unwrap();
	assert!(workers >= 2, "{main:?}");
	assert_ne!(main["worker-1"], "0");
	assert_ne!(main["worker-2"], "0");
	assert_ne!(main["worker-1"], main["caller-thread"]);
	assert_ne!(main["worker-2"], main["caller-thread"]);
	assert_ne!(main["worker-1"], main["worker-2"]);
	assert_eq!(main["province-count"], "128");
	assert_eq!(main["provinces-on-workers"], main["province-count"]);
	assert_eq!(main["caller-computed"], "0");
	assert_eq!(main["joined"], main["serial"]);
	assert_ne!(main["serial"], "0");
	assert_eq!(main["original-after-join"], "1");
	assert_eq!(main["workers-before-original"], main["worker-count"]);
	assert_eq!(main["original-thread"], main["caller-thread"]);
	assert_eq!(main["snapshot"], main["joined"]);
	assert_ne!(main["snapshot-thread"], main["caller-thread"]);
	assert_ne!(main["snapshot-thread"], "0");
	assert_eq!(main["entries-before-snapshot"], "1");
	assert_eq!(main["post-main-entries"], "1");
	assert_eq!(main["entries-after-snapshot"], "1");
	assert_eq!(late["deferred-enable"], "-2");
	assert_eq!(late["deferred-bytes"], "unchanged");
}

#[test]
fn country_states_run_across_threads_before_monthly_resumes() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run("countries");
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
	let entry = fs::read_to_string(rt.path("foch-host/countries-entry.txt")).unwrap();
	let main = fs::read_to_string(rt.path("foch-host/countries.txt")).unwrap();
	let entry = proof(&entry);
	let main = proof(&main);
	assert_eq!(entry["found"], "0", "{entry:?}");
	assert_eq!(entry["created"], "0", "{entry:?}");
	assert_eq!(entry["enabled"], "0", "{entry:?}");
	assert_ne!(entry["monthly"], "0", "{entry:?}");
	assert_eq!(entry["state-found"], "0", "{entry:?}");
	assert_eq!(entry["state-created"], "0", "{entry:?}");
	assert_eq!(entry["state-enabled"], "0", "{entry:?}");
	assert_ne!(entry["state"], "0", "{entry:?}");
	assert_eq!(main["span"], "73", "{main:?}");
	assert_eq!(main["pattern"], "ok", "{main:?}");
	assert_eq!(main["redirect"], "ff25", "{main:?}");
	assert_eq!(main["country-count"], "32");
	assert_eq!(main["hits-at-join"], "32");
	assert_eq!(main["monthly-on-caller"], "32");
	assert_eq!(main["args"], "ok");
	assert_eq!(main["state-count"], "64");
	assert_eq!(main["state-hits"], "64");
	assert_eq!(main["state-on-caller"], "0");
	assert_eq!(main["state-same"], "ok");
	let workers: i32 = main["state-worker-count"].parse().unwrap();
	assert!(workers >= 2, "{main:?}");
	assert_ne!(main["state-worker-1"], "0");
	assert_ne!(main["state-worker-2"], "0");
	assert_ne!(main["state-worker-1"], main["caller-thread"]);
	assert_ne!(main["state-worker-2"], main["caller-thread"]);
	assert_ne!(main["state-worker-1"], main["state-worker-2"]);
	assert_eq!(entry["active-found"], "0", "{entry:?}");
	assert_eq!(entry["active-created"], "0", "{entry:?}");
	assert_eq!(entry["active-enabled"], "0", "{entry:?}");
	assert_ne!(entry["active"], "0", "{entry:?}");
}

#[test]
fn active_country_loop_matches_with_and_without_the_hook() {
	let rt = Runtime::new();
	let _ = rt.run("active");
	let plain =
		fs::read_to_string(rt.path("foch-host/active-game.txt")).expect("unhooked active proof");
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	// Judging one fixture country is a handful of loads, so a busy machine can
	// let one pool thread drain every part before a second thread starts. The
	// proof must still show a real spread; retry the whole launch instead of
	// weakening the assertion.
	let mut events = Vec::new();
	for attempt in 0..3 {
		events = rt.run("active");
		let workers_text = fs::read_to_string(rt.path("foch-host/active.txt")).unwrap();
		let early = proof(&workers_text);
		let worker_count: i32 = early["worker-count"].parse().unwrap();
		if worker_count >= 2 || attempt == 2 {
			break;
		}
	}
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
	let hooked = fs::read_to_string(rt.path("foch-host/active-game.txt")).unwrap();
	let entry = fs::read_to_string(rt.path("foch-host/countries-entry.txt")).unwrap();
	let workers = fs::read_to_string(rt.path("foch-host/active.txt")).unwrap();
	let plain = proof(&plain);
	let hooked = proof(&hooked);
	let entry = proof(&entry);
	let workers = proof(&workers);
	assert_eq!(plain["redirect"], "other", "{plain:?}");
	assert_eq!(hooked["redirect"], "ff25", "{hooked:?}");
	assert_eq!(plain["match"], "ok", "{plain:?}");
	assert_eq!(hooked["match"], "ok", "{hooked:?}");
	assert_eq!(plain["sum"], "8000", "{plain:?}");
	assert_eq!(hooked["sum"], plain["sum"], "{hooked:?} {plain:?}");
	assert_eq!(hooked["tags"], plain["tags"], "{hooked:?} {plain:?}");
	assert_eq!(hooked["ptrs"], plain["ptrs"], "{hooked:?} {plain:?}");
	assert_eq!(plain["tag-count"], "20");
	assert_eq!(plain["ptr-count"], "8");
	assert_eq!(hooked["tag-count"], plain["tag-count"]);
	assert_eq!(hooked["ptr-count"], plain["ptr-count"]);
	assert_eq!(plain["pred-caller"], "12");
	assert_eq!(plain["pred-other"], "0");
	assert_eq!(hooked["pred-caller"], "12");
	assert_eq!(hooked["pred-other"], "0");
	assert_eq!(hooked["aux"], plain["aux"]);
	assert_eq!(entry["active-found"], "0", "{entry:?}");
	assert_eq!(entry["active-created"], "0", "{entry:?}");
	assert_eq!(entry["active-enabled"], "0", "{entry:?}");
	assert_ne!(entry["active"], "0");
	assert_eq!(entry["active-real"], "0", "{entry:?}");
	let trampoline = fs::read_to_string(rt.path("foch-host/active-trampoline.txt")).unwrap();
	let trampoline = proof(&trampoline);
	assert_eq!(trampoline["match"], "ok", "{trampoline:?}");
	assert_eq!(trampoline["sum"], "3000", "{trampoline:?}");
	assert_eq!(trampoline["copy-sum"], "3000");
	assert_eq!(trampoline["body-hash"], "7747e17d");
	assert_eq!(trampoline["aux-calls"], "5");
	assert_eq!(trampoline["query-calls"], "2");
	assert_eq!(trampoline["fallback-calls"], "2");
	assert_eq!(trampoline["keep-calls"], "3");
	assert_eq!(trampoline["other-calls"], "0");
	assert_eq!(trampoline["alloc-calls"], "0");
	assert_eq!(trampoline["assign-calls"], "0");
	assert_eq!(trampoline["grow-calls"], "0");
	assert_eq!(trampoline["tag-count"], "5");
	assert_eq!(trampoline["ptr-count"], "3");
	assert_eq!(workers["sum"], "8000", "{workers:?}");
	assert_eq!(workers["count"], "32");
	assert_eq!(workers["judged"], "32");
	assert_eq!(workers["on-caller"], "0", "{workers:?}");
	assert_eq!(workers["commit-thread"], hooked["caller"], "{workers:?}");
	let worker_count: i32 = workers["worker-count"].parse().unwrap();
	assert!(worker_count >= 2, "{workers:?}");
	assert_ne!(workers["worker-1"], "0");
	assert_ne!(workers["worker-2"], "0");
	assert_ne!(workers["worker-1"], workers["commit-thread"]);
	assert_ne!(workers["worker-2"], workers["commit-thread"]);
	assert_ne!(workers["worker-1"], workers["worker-2"]);
}

fn proof(text: &str) -> std::collections::BTreeMap<&str, &str> {
	text.lines()
		.filter_map(|line| line.split_once('='))
		.collect()
}

#[test]
fn daily_trace_leaves_unowned_exception_contexts_unchanged() {
	let rt = Runtime::new();
	rt.plan(Vec::new());
	rt.run("dailya-trace-unowned");
	let text = fs::read_to_string(rt.path("foch-host/trace-unowned.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["guard-disposition"], "0", "{rows:?}");
	assert_eq!(rows["guard-flags"], "514", "{rows:?}");
	assert_eq!(rows["step-disposition"], "0", "{rows:?}");
}

#[test]
fn daily_trace_records_and_stops_before_releasing_its_resources() {
	let rt = Runtime::new();
	rt.plan(Vec::new());
	rt.run("dailya-trace");
	let text = fs::read_to_string(rt.path("foch-host/trace-lifecycle.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["counter"], "6", "{rows:?}");
	assert!(rows["steps"].parse::<usize>().unwrap() > 0, "{rows:?}");
	assert_eq!(rows["removed"], "2", "{rows:?}");
	assert_eq!(rows["trap-flag"], "0", "{rows:?}");
	assert_eq!(rows["foreign-owner"], "4", "{rows:?}");
	assert_eq!(rows["foreign-worker"], "2", "{rows:?}");
	assert_eq!(rows["callbacks-after-stop"], "0", "{rows:?}");
	let steps = fs::read_to_string(rt.path("foch-host/trace-lifecycle-steps.txt")).unwrap();
	assert!(steps.lines().any(|line| line.contains("rax=")), "{steps}");
}

#[test]
fn daily_serial_baseline_preserves_the_windows_stack_contract() {
	let rt = Runtime::new();
	rt.plan(Vec::new());
	rt.run("dailya-serial");
	let text = fs::read_to_string(rt.path("foch-host/daily-a-serial-abi.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["entry-rsp-mod16"], "8", "{rows:?}");
	assert_eq!(rows["serial-count"], "0", "{rows:?}");
	assert_eq!(rows["serial-nodes"], "37", "{rows:?}");
	assert_eq!(rows["ordered"], "1", "{rows:?}");
	assert_eq!(rows["payloads"], "1", "{rows:?}");
}

#[test]
fn daily_fallback_preserves_the_native_head_inputs_and_ownership() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run("dailya-fallback");
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
	let text = fs::read_to_string(rt.path("foch-host/daily-a-fallback-abi.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["unready"], "0", "{rows:?}");
	assert_eq!(rows["nested"], "0", "{rows:?}");
	assert_eq!(rows["unready-owner"], "0", "{rows:?}");
	assert_eq!(rows["nested-owner"], "1", "{rows:?}");
	assert!(!rt.path("foch-host/daily-a-dispatch.txt").exists());
}

#[test]
fn known_monthly_native_callback_cadence_and_frame_match_the_hook() {
	for mode in ["parallel", "serial", "parts", "pool", "work"] {
		let rt = Runtime::new();
		let _ = rt.plugin("eu4_country_month", "native", "entry");
		rt.plan(vec![]);
		rt.run(&format!("monthly-callback-{mode}"));
		let text = fs::read_to_string(rt.path("foch-host/monthly-callback.txt")).unwrap();
		let rows = proof(&text);
		assert_eq!(rows["native-first"], "7", "{mode}: {rows:?}");
		assert_eq!(rows["hook-first"], "7", "{mode}: {rows:?}");
		assert_eq!(rows["errors"], "0", "{mode}: {rows:?}");
		assert_eq!(rows["variants"], "3", "{mode}: {rows:?}");
		println!("monthly {mode}: {rows:?}");
	}
}

#[test]
fn known_complete_monthly_and_active_profiles_still_install() {
	let probe = Runtime::new();
	let _ = probe.plugin("eu4_country_month", "native", "entry");
	probe.plan(vec![]);
	probe.run("member-known-profiles");
	let text = fs::read_to_string(probe.path("foch-host/known-profiles.txt")).unwrap();
	let fields = proof(&text);
	assert_eq!(fields["init-rc"], "0", "{fields:?}");
	assert_eq!(fields["state"], "2", "{fields:?}");
	assert_eq!(fields["created"], "3", "{fields:?}");
	for field in [
		"active-real",
		"copy-built",
		"monthly-profile",
		"active-header",
	] {
		assert_eq!(fields[field], "1", "{fields:?}");
	}
	for member in ["monthly", "active"] {
		let rt = Runtime::new();
		rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
		let events = rt.run(&format!("profile-{member}-known"));
		assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
		let text = fs::read_to_string(rt.path("foch-host/init-fault.txt")).unwrap();
		let fields = proof(&text);
		assert_eq!(fields["init-rc"], "0", "{fields:?}");
		assert_eq!(fields["enabled"], "7", "{fields:?}");
		let path = if member == "monthly" {
			"profile-monthly.txt"
		} else {
			"active-game.txt"
		};
		let text = fs::read_to_string(rt.path(&format!("foch-host/{path}"))).unwrap();
		let fields = proof(&text);
		let counter = if member == "monthly" {
			"tail-counter"
		} else {
			"header-counter"
		};
		assert_eq!(fields[counter], "0", "{fields:?}");
	}
}

#[test]
fn monthly_skipped_tail_with_an_effect_refuses_before_any_enable() {
	check_skipped_profile("monthly", "32");
}

#[test]
fn active_skipped_header_with_an_effect_refuses_before_any_enable() {
	check_skipped_profile("active", "1");
}

fn check_skipped_profile(member: &str, expected: &str) {
	for selected in [false, true] {
		let rt = Runtime::new();
		if selected {
			rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
		} else {
			rt.plan(vec![]);
		}
		let events = rt.run(&format!(
			"profile-{member}-{}",
			if selected { "gap" } else { "native" }
		));
		let path = if member == "monthly" {
			"profile-monthly.txt"
		} else {
			"active-game.txt"
		};
		let text = fs::read_to_string(rt.path(&format!("foch-host/{path}"))).unwrap();
		let rows = proof(&text);
		let field = if member == "monthly" {
			"tail-counter"
		} else {
			"header-counter"
		};
		assert_eq!(
			rows[field], expected,
			"member={member} selected={selected}: {rows:?}"
		);
		if member == "monthly" {
			assert_eq!(rows["countries"], "32", "{rows:?}");
			assert_eq!(rows["states"], "64", "{rows:?}");
			assert_eq!(rows["caller-states"], "64", "{rows:?}");
			assert_eq!(rows["args-ok"], "1", "{rows:?}");
		}
		if selected {
			assert_eq!(states(&events, "eu4_country_month").last(), Some(&"failed"));
			let text = fs::read_to_string(rt.path("foch-host/init-fault.txt")).unwrap();
			let fields = proof(&text);
			assert_ne!(fields["init-rc"], "0", "{fields:?}");
			assert_eq!(fields["plugin-status"], "5", "{fields:?}");
			assert_eq!(fields["enable-attempts"], "0", "{fields:?}");
			assert_eq!(fields["enabled"], "0", "{fields:?}");
			assert_eq!(fields["raw-unchanged"], fields["created"], "{fields:?}");
			println!(
				"{member}: native-counter={expected} refused={} enables={} live={}",
				fields["init-rc"], fields["enable-attempts"], fields["enabled"]
			);
		}
	}
}

#[test]
fn country_active_preparation_failures_preserve_original_native_loops() {
	check_country_init_fault("active-create");
	check_country_init_fault("active-shape");
}

#[test]
fn country_init_failures_leave_native_behavior_and_no_enabled_member_hooks() {
	for phase in [
		"state", "country", "active", "alloc", "free", "end", "daily",
	] {
		for action in ["create", "enable", "enable-after"] {
			check_country_init_fault(&format!("{phase}-{action}"));
		}
	}
	check_country_init_fault("active-shape");
}

fn check_country_init_fault(fault: &str) {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run(&format!("init-fault-{fault}"));
	assert_eq!(
		states(&events, "eu4_country_month").last(),
		Some(&"failed"),
		"{fault}"
	);
	let text = fs::read_to_string(rt.path("foch-host/init-fault.txt")).unwrap();
	let fields = proof(&text);
	assert_ne!(fields["init-rc"], "0", "{fault}: {fields:?}");
	assert_eq!(fields["plugin-status"], "5", "{fault}: {fields:?}");
	assert_eq!(fields["reason"], fields["init-rc"], "{fault}: {fields:?}");
	assert_eq!(fields["enabled"], "0", "{fault}: {fields:?}");
	assert_eq!(fields["rollback-errors"], "0", "{fault}: {fields:?}");
	if fault.contains("enable") {
		assert_eq!(
			fields["disable-order"], "free,alloc,daily,end,active,country,state",
			"{fault}: {fields:?}"
		);
	}
	assert_eq!(
		fields["original-prefix"], fields["created"],
		"{fault}: {fields:?}"
	);
	if fault.ends_with("create") || fault == "active-shape" {
		assert_eq!(
			fields["raw-unchanged"], fields["created"],
			"{fault}: {fields:?}"
		);
	}
	if fault.starts_with("active-") && (fault.ends_with("create") || fault == "active-shape") {
		assert_eq!(fields["daily-created"], "0", "{fault}: {fields:?}");
	}
	let text = fs::read_to_string(rt.path("foch-host/init-native.txt")).unwrap();
	assert_eq!(proof(&text)["errors"], "0", "{fault}: {text}");
	println!(
		"fault={fault} status={} created={} live={} raw={} original={} rollback={} native-errors=0",
		fields["plugin-status"],
		fields["created"],
		fields["enabled"],
		fields["raw-unchanged"],
		fields["original-prefix"],
		fields["rollback-errors"]
	);
}

#[test]
fn country_init_reports_rollback_failure_and_remaining_live_hook() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run("init-fault-active-enable-rollback");
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"failed"));
	let text = fs::read_to_string(rt.path("foch-host/init-fault.txt")).unwrap();
	let fields = proof(&text);
	assert_eq!(fields["init-rc"], "-4", "{fields:?}");
	assert_eq!(fields["plugin-status"], "5", "{fields:?}");
	assert_eq!(fields["reason"], "-4", "{fields:?}");
	assert_eq!(
		fields["detail"], "country month rollback failed",
		"{fields:?}"
	);
	assert_eq!(fields["rollback-errors"], "1", "{fields:?}");
	assert_eq!(fields["enabled"], "1", "{fields:?}");
	assert!(
		fields["disable-order"].ends_with(",country,state"),
		"{fields:?}"
	);
}

#[test]
fn monthly_truncated_profile_refuses_without_reading_the_next_page() {
	let rt = Runtime::new();
	let _ = rt.plugin("eu4_country_month", "native", "entry");
	rt.plan(Vec::new());
	rt.run("member-edge-monthly");
	let text = fs::read_to_string(rt.path("foch-host/member-edges.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["refused"], rows["expected"], "{rows:?}");
	assert_eq!(rows["preserved"], rows["expected"], "{rows:?}");
}

#[test]
fn active_truncated_profile_refuses_without_reading_the_next_page() {
	let rt = Runtime::new();
	let _ = rt.plugin("eu4_country_month", "native", "entry");
	rt.plan(Vec::new());
	rt.run("member-edge-active");
	let text = fs::read_to_string(rt.path("foch-host/member-edges.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["refused"], rows["expected"], "{rows:?}");
	assert_eq!(rows["preserved"], rows["expected"], "{rows:?}");
}

#[test]
fn active_relative_targets_are_checked_before_reading_globals_or_magic() {
	let rt = Runtime::new();
	let _ = rt.plugin("eu4_country_month", "native", "entry");
	rt.plan(Vec::new());
	rt.run("member-edge-target");
	let text = fs::read_to_string(rt.path("foch-host/member-edges.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["refused"], rows["expected"], "{rows:?}");
	assert_eq!(rows["preserved"], rows["expected"], "{rows:?}");
}

#[test]
fn active_actual_body_with_an_extra_call_is_refused_before_relocation() {
	let rt = Runtime::new();
	let _ = rt.plugin("eu4_country_month", "native", "entry");
	rt.plan(Vec::new());
	rt.run("member-active-extra");
	let text = fs::read_to_string(rt.path("foch-host/active-profile.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["known-supported"], "1", "{rows:?}");
	assert_eq!(rows["original-returned"], "1", "{rows:?}");
	assert_eq!(rows["extra-refused"], "1", "{rows:?}");
	assert_eq!(rows["init-refused"], "1", "{rows:?}");
}

#[test]
fn daily_large_operation_log_is_fully_reserved_before_country_work() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-large-ops");
	let text = fs::read_to_string(rt.path("foch-host/daily-large-ops.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["errors"], "0", "{rows:?}");
	assert_eq!(rows["ops-allocations"], "1", "{rows:?}");
}

#[test]
fn daily_preflight_failures_execute_the_native_path_exactly_once() {
	let mut failures = Vec::new();
	for resource in [
		"body",
		"parts",
		"publish-table",
		"snapshot",
		"shadow",
		"arena",
		"arena-reserve",
		"arena-commit",
		"ops",
		"publish-node",
		"late-parts",
		"late-snapshot",
		"late-shadow",
		"late-arena",
		"late-arena-reserve",
		"late-arena-commit",
		"late-ops",
		"late-publish-node",
	] {
		let rt = Runtime::new();
		rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
		rt.run(&format!("dailya-fail-{resource}"));
		let text = fs::read_to_string(rt.path("foch-host/daily-failure.txt")).unwrap();
		let rows = proof(&text);
		if rows["errors"] != "0"
			|| rows["fallback"] != "1"
			|| rows["hits"] == "0"
			|| rows["resumed"] != "1"
			|| rows["foreign-free"] != "0"
		{
			failures.push(format!("{resource}: {rows:?}"));
		}
	}
	assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn daily_install_refuses_truncated_unreadable_and_overflowing_images() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-range");
	let text = fs::read_to_string(rt.path("foch-host/daily-range.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["refused"], "4", "{rows:?}");
	assert_eq!(rows["noaccess-preserved"], "1", "{rows:?}");
	assert_eq!(rows["guard-preserved"], "1", "{rows:?}");
}

#[test]
fn daily_arena_ownership_survives_multiple_real_dispatch_rounds() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-arena-rounds");
	let text = fs::read_to_string(rt.path("foch-host/daily-rounds.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["errors"], "0", "{rows:?}");
	assert_eq!(rows["foreign-free"], "0", "{rows:?}");
	assert_eq!(
		rows["owners-after-parallel"], rows["owners-final"],
		"{rows:?}"
	);
}

#[test]
fn daily_arena_accounting_reports_bounded_reuse_and_zero_live_after_multi_round_runs() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run("dailya-arena-rounds");
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
	let rounds_text = fs::read_to_string(rt.path("foch-host/daily-rounds.txt")).unwrap();
	let rounds = proof(&rounds_text);
	assert_eq!(rounds["errors"], "0", "{rounds:?}");
	assert_eq!(rounds["foreign-free"], "0", "{rounds:?}");
	let owners_final: u64 = rounds["owners-final"].parse().unwrap();
	let live_text = fs::read_to_string(rt.path("foch-host/daily-a-real.txt")).unwrap();
	let live = proof(&live_text);
	assert_eq!(live["calls"], "5", "{live:?}");
	let workers: u64 = live["workers"].parse().unwrap();
	assert!(workers >= 2, "{live:?}");
	let arena_count: u64 = live["arena-count"].parse().unwrap();
	let allocs: u64 = live["arena-allocs"].parse().unwrap();
	let frees: u64 = live["arena-frees"].parse().unwrap();
	let live_allocs: u64 = live["arena-live"].parse().unwrap();
	let peak_live: u64 = live["arena-peak-live"].parse().unwrap();
	let committed: u64 = live["arena-committed-bytes"].parse().unwrap();
	let oversized: u64 = live["arena-oversized"].parse().unwrap();
	assert_eq!(arena_count, owners_final, "{live:?} {rounds:?}");
	assert_eq!(arena_count, workers, "{live:?}");
	assert_eq!(allocs, 65, "{live:?}");
	assert_eq!(frees, allocs, "{live:?}");
	assert_eq!(live_allocs, 0, "{live:?}");
	assert!(peak_live >= 1 && peak_live <= arena_count, "{live:?}");
	assert_eq!(committed, arena_count * 0x400000, "{live:?}");
	assert_eq!(oversized, 0, "{live:?}");
}

#[test]
fn daily_arena_never_returns_an_unregistered_thirty_third_owner() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-arena-owner");
	let text = fs::read_to_string(rt.path("foch-host/daily-arena.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["result"], "32", "{rows:?}");
	assert_eq!(rows["foreign-free"], "0", "{rows:?}");
}

#[test]
fn daily_arena_reuse_keeps_size_metadata_separate_from_freelist_links() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-arena-reuse");
	let text = fs::read_to_string(rt.path("foch-host/daily-arena.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["result"], "3", "{rows:?}");
	assert_eq!(rows["foreign-free"], "0", "{rows:?}");
}

#[test]
fn daily_arena_boundary_sizes_use_matching_native_allocation_provenance() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-arena-boundary");
	let text = fs::read_to_string(rt.path("foch-host/daily-arena.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["result"], "6", "{rows:?}");
	assert_eq!(rows["native-free"], "5", "{rows:?}");
	assert_eq!(rows["foreign-free"], "0", "{rows:?}");
}

#[test]
fn daily_relocation_does_not_decode_displacement_bytes_as_calls() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-relocation");
	let text = fs::read_to_string(rt.path("foch-host/daily-relocation.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["call-target"], "1", "{rows:?}");
	assert_eq!(rows["unknown-refused"], "1", "{rows:?}");
}

#[test]
fn daily_worker_reserves_the_native_callee_home_space() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-home");
	let text = fs::read_to_string(rt.path("foch-host/daily-slice.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["frame-errors"], "0", "{rows:?}");
	assert_eq!(rows["frame-negative"], "16", "{rows:?}");
}

#[test]
fn daily_builder_preserves_the_native_head_after_hook_installation() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-build");
	let text = fs::read_to_string(rt.path("foch-host/daily-builder.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["native-head"], "1", "{rows:?}");
}

#[test]
fn daily_builder_encodes_branches_without_overwriting_their_opcodes() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-build");
	let text = fs::read_to_string(rt.path("foch-host/daily-builder.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["branch-errors"], "0", "{rows:?}");
}

#[test]
fn daily_builder_removal_resumes_at_the_fixture_loop_decrement() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-build");
	let text = fs::read_to_string(rt.path("foch-host/daily-builder.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["tail-decrement"], "5056", "{rows:?}");
}

#[test]
fn daily_builder_resolves_the_wired_native_replay_targets() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-build");
	let text = fs::read_to_string(rt.path("foch-host/daily-builder.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["replay-targets"], "1", "{rows:?}");
}

#[test]
fn daily_worker_stops_at_its_assigned_lower_queue_index() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-slice");
	let text = fs::read_to_string(rt.path("foch-host/daily-slice.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["outside-hits"], "0", "{rows:?}");
	assert_eq!(rows["ops"], "6", "{rows:?}");
}

#[test]
fn daily_shadow_shift_updates_the_native_header_count() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-header");
	let text = fs::read_to_string(rt.path("foch-host/daily-header.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["count"], "2", "{rows:?}");
	assert_eq!(rows["shifted"], "1", "{rows:?}");
}

#[test]
fn daily_removal_stub_records_the_native_queue_index() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-slice");
	let text = fs::read_to_string(rt.path("foch-host/daily-slice.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["removal-errors"], "0", "{rows:?}");
}

#[test]
fn daily_publish_stub_records_the_native_country_record() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-slice");
	let text = fs::read_to_string(rt.path("foch-host/daily-slice.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["publish-errors"], "0", "{rows:?}");
}

#[test]
fn daily_list_comparison_uses_country_identity_and_exact_order_and_count() {
	let rt = Runtime::new();
	rt.plan(Vec::new());
	rt.run("dailya-compare");
	let text = fs::read_to_string(rt.path("foch-host/daily-list-comparison.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["same"], "1", "{rows:?}");
	for rejected in ["reordered", "short-list", "unknown", "wrong-count"] {
		assert_eq!(rows[rejected], "0", "{rows:?}");
	}
}

#[test]
fn daily_built_worker_preserves_its_return_frame_and_nonvolatile_registers() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	rt.run("dailya-slice");
	let text = fs::read_to_string(rt.path("foch-host/daily-slice.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["frame-errors"], "0", "{rows:?}");
	assert_eq!(rows["frame-negative"], "16", "{rows:?}");
}

#[test]
fn daily_worker_enters_with_the_native_queue_index_and_count() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run("dailya-worker-abi");
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
	let text = fs::read_to_string(rt.path("foch-host/daily-worker-abi.txt")).unwrap();
	let rows = proof(&text);
	assert_eq!(rows["cases"], "2", "{rows:?}");
	assert_eq!(rows["entry-errors"], "0", "{rows:?}");
}

#[test]
fn daily_queue_takeover_matches_the_serial_baseline() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run("dailya");
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
	let entry = fs::read_to_string(rt.path("foch-host/countries-entry.txt")).unwrap();
	let entry = proof(&entry);
	assert_ne!(entry["daily-a"], "0", "{entry:?}");
	assert_eq!(entry["daily-a-ready"], "1", "{entry:?}");
	let da = fs::read_to_string(rt.path("foch-host/daily-a-fixture.txt")).unwrap();
	let da = proof(&da);
	assert_eq!(da["match"], "ok", "{da:?}");
	assert_eq!(da["serial-count"], da["parallel-count"], "{da:?}");
	assert_eq!(da["serial-nodes"], da["parallel-nodes"], "{da:?}");
	assert_eq!(da["payloads"], "ok", "{da:?}");
	assert_eq!(da["warm-match"], "1", "{da:?}");
	assert_eq!(da["warm-calls"], "1", "{da:?}");
	assert_eq!(da["warm-workers"], "1", "{da:?}");
	let live_text = fs::read_to_string(rt.path("foch-host/daily-a-real.txt"));
	assert!(live_text.is_ok(), "{da:?}");
	let live_binding = live_text.unwrap();
	let live = proof(&live_binding);
	assert_eq!(live["calls"], "2", "{live:?}");
	let workers: i32 = live["workers"].parse().unwrap();
	assert!(workers >= 2, "{live:?}");
	assert!(live["ops"].parse::<u32>().unwrap() > 0, "{live:?}");
	eprintln!("daily differential: {da:?}; parallel: {live:?}");
}

#[test]
fn active_timing_reports_the_hook_and_plain_spans() {
	let rt = Runtime::new();
	let _ = rt.run("active-timing");
	let plain = fs::read_to_string(rt.path("foch-host/active-timing.txt"))
		.expect("plain active timing proof");
	let plain = proof(&plain);
	assert_eq!(plain["hooked"], "0", "{plain:?}");
	assert_eq!(plain["rounds"], "4000", "{plain:?}");
	let plain_loop: i64 = plain["median-loop-ns"].parse().unwrap();
	assert!(plain_loop > 0, "{plain:?}");

	rt.plan(vec![rt.plugin("eu4_country_month", "native", "entry")]);
	let events = rt.run("active-timing");
	assert_eq!(states(&events, "eu4_country_month").last(), Some(&"active"));
	let hooked = fs::read_to_string(rt.path("foch-host/active-timing.txt")).unwrap();
	let hooked = proof(&hooked);
	assert_eq!(hooked["hooked"], "1", "{hooked:?}");
	let judge: i64 = hooked["median-judge-ns"].parse().unwrap();
	let writeback: i64 = hooked["median-writeback-ns"].parse().unwrap();
	let total: i64 = hooked["median-total-ns"].parse().unwrap();
	let gross_loop: i64 = hooked["median-loop-ns"].parse().unwrap();
	assert!(judge > 0, "{hooked:?}");
	assert!(writeback > 0, "{hooked:?}");
	assert!(total >= judge + writeback, "{hooked:?}");
	assert!(gross_loop > 0, "{hooked:?}");
	// A report, not a gate: whether the pool pays for 32 fixture judgments
	// is the machine's call, so the ratio is printed, not asserted.
	println!(
		"active timing: hooked total {} ns vs plain loop {} ns (ratio {:.3}); judge {} ns, writeback {} ns, gross loop {} ns",
		total,
		plain_loop,
		total as f64 / plain_loop as f64,
		judge,
		writeback,
		gross_loop
	);
}

#[test]
fn proxy_forwards_without_a_plan_and_ignores_unlisted_dlls() {
	let rt = Runtime::new();
	assert!(rt.run("plain").is_empty());
	fs::remove_file(rt.path("foch-runtime")).unwrap();
	rt.plan(Vec::new());
	assert!(rt.run("forward-only").is_empty());
	assert!(!rt.path("foch-host/events.jsonl").exists());
	assert!(!rt.path("foch-host/host-error.txt").exists());
}

#[test]
fn native_initializes_before_main_and_reports_configuration_and_state() {
	let rt = Runtime::new();
	// A packaged auxiliary DLL is not necessarily an entry DLL import and
	// must not prevent loading the selected entry or be discovered implicitly.
	fs::create_dir(rt.path("plugins/resources")).unwrap();
	fs::copy(
		fixtures().path().join("native.dll"),
		rt.path("plugins/resources/auxiliary.dll"),
	)
	.unwrap();
	rt.plan(vec![rt.plugin("native", "native", "entry")]);
	let events = rt.run("native");
	assert_eq!(
		&states(&events, "native")[..3],
		&["loading", "dll_loaded", "initializing"]
	);
	assert!(states(&events, "native").contains(&"active"));
	assert!(
		events
			.iter()
			.any(|v| v["event"] == "log" && v["message"] == "fixture init")
	);
	assert!(events.iter().all(|v| v["run_id"] == "fixture-run"));
}

#[test]
fn diagnostic_active_state_cannot_activate_a_distinct_pact_identity() {
	let rt = Runtime::new();
	// These are compiled C fixtures, not EU4 or a successful pact adapter.
	// The real host must attribute their states to the plan's distinct IDs.
	let pact_id = "dev.foch.directed-pacts";
	let probe_id = "dev.foch.directed-pacts.roster-probe";
	let mut refused = rt.plugin("bad_abi", "native", "entry");
	refused["id"] = pact_id.into();
	let mut diagnostic = rt.plugin("native", "native", "entry");
	diagnostic["id"] = probe_id.into();
	rt.plan(vec![refused, diagnostic]);
	let events = rt.run("plain");
	assert_eq!(states(&events, pact_id).last(), Some(&"refused"));
	assert!(!states(&events, pact_id).contains(&"active"));
	assert_eq!(states(&events, probe_id).last(), Some(&"active"));
	assert!(!states(&events, "native").contains(&"active"));
}

#[test]
fn legacy_sync_poll_refusal_and_timeout_are_actual_states() {
	let rt = Runtime::new();
	let mut plugins = Vec::new();
	for (name, mode) in [
		("sync", "sync"),
		("poll", "poll"),
		("refused", "sync"),
		("timeout", "poll"),
	] {
		let mut plugin = rt.plugin(
			name,
			"legacy",
			if name == "sync" { "entry" } else { "deferred" },
		);
		plugin["status"] = json!({"export":"ProbeState", "mode":mode, "timeout_ms":if name == "timeout" {150} else {3000},
			"pending":[0], "values":[{"value":1,"state":"active"}, {"value":-3,"state":"refused","reason":"foreign modification"}]});
		plugins.push(plugin);
	}
	rt.plan(plugins);
	let events = rt.run("plain");
	assert_eq!(states(&events, "sync").last(), Some(&"active"));
	assert_eq!(states(&events, "poll").last(), Some(&"active"));
	assert_eq!(states(&events, "refused").last(), Some(&"refused"));
	assert_eq!(states(&events, "timeout").last(), Some(&"unknown"));
	let order: Vec<_> = events
		.iter()
		.filter(|v| v["state"] == "loading")
		.map(|v| v["plugin_id"].as_str().unwrap())
		.collect();
	assert_eq!(order, ["sync", "poll", "refused", "timeout"]);
}

#[test]
fn rejects_bad_native_tables_without_calling_init_and_continues() {
	let rt = Runtime::new();
	rt.plan(
		[
			"bad_abi",
			"short_api",
			"null_init",
			"null_api",
			"init_error",
			"native",
		]
		.into_iter()
		.map(|name| rt.plugin(name, "native", "entry"))
		.collect(),
	);
	let events = rt.run("native");
	for id in ["bad_abi", "short_api", "null_init", "null_api"] {
		assert_eq!(states(&events, id).last(), Some(&"refused"), "{id}");
	}
	assert_eq!(states(&events, "init_error").last(), Some(&"failed"));
	assert_eq!(states(&events, "native").last(), Some(&"active"));
}

#[test]
fn missing_exports_and_digest_mismatch_do_not_become_active() {
	let rt = Runtime::new();
	let mut missing = rt.plugin("sync", "legacy", "entry");
	missing["status"] = json!({"export":"MissingExport","mode":"sync","values":[]});
	let mut mismatch = rt.plugin("native", "native", "entry");
	mismatch["sha256"] = "0".repeat(64).into();
	rt.plan(vec![missing, mismatch]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "sync").last(), Some(&"unknown"));
	assert_eq!(states(&events, "native").last(), Some(&"refused"));
	assert!(!states(&events, "native").contains(&"dll_loaded"));
}

#[test]
fn invalid_plan_leaves_proxy_operational_and_writes_a_diagnostic() {
	let rt = Runtime::new();
	fs::write(rt.path("foch-host/plan.json"), "{broken").unwrap();
	assert!(rt.run("plain").is_empty());
	assert!(rt.path("foch-host/host-error.txt").is_file());
}

#[test]
fn event_files_cannot_redirect_host_writes_outside_the_runtime() {
	let original = "{\"guard\":\"original bytes\"}\n";
	for mode in ["existing", "missing", "hardlink"] {
		let rt = Runtime::new();
		let outside = tempfile::tempdir().unwrap();
		let destination = outside.path().join("guarded-events.jsonl");
		if mode != "missing" {
			fs::write(&destination, original).unwrap();
		}
		rt.plan(vec![rt.plugin("native", "native", "entry")]);
		if mode == "hardlink" {
			fs::hard_link(&destination, rt.path("foch-host/events.jsonl")).unwrap();
		} else {
			let mut plan: Value =
				serde_json::from_slice(&fs::read(rt.path("foch-host/plan.json")).unwrap()).unwrap();
			plan["events"] = destination.to_str().unwrap().into();
			fs::write(
				rt.path("foch-host/plan.json"),
				serde_json::to_vec(&plan).unwrap(),
			)
			.unwrap();
		}
		rt.run("forward-only");
		if mode == "missing" {
			assert!(!destination.exists(), "external event file created");
		} else {
			assert_eq!(
				fs::read_to_string(&destination).unwrap(),
				original,
				"{mode}"
			);
		}
		assert!(rt.path("foch-host/host-error.txt").is_file(), "{mode}");
	}
}

#[test]
fn diagnostic_files_cannot_overwrite_an_external_hardlink() {
	let rt = Runtime::new();
	let outside = tempfile::tempdir().unwrap();
	let destination = outside.path().join("guarded-diagnostic.txt");
	fs::write(&destination, "original bytes").unwrap();
	fs::hard_link(&destination, rt.path("foch-host/host-error.txt")).unwrap();
	fs::write(rt.path("foch-host/plan.json"), "{broken").unwrap();
	assert!(rt.run("forward-only").is_empty());
	assert_eq!(fs::read_to_string(destination).unwrap(), "original bytes");
}

#[test]
fn asynchronous_native_callbacks_keep_the_host_context_alive() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("async_native", "native", "deferred")]);
	let events = rt.run("delayed");
	assert!(states(&events, "async_native").contains(&"initializing"));
	assert_eq!(states(&events, "async_native").last(), Some(&"active"));
}

#[test]
fn deferred_loading_waits_for_pending_entry_initialization() {
	let rt = Runtime::new();
	let mut deferred = rt.plugin("sync", "legacy", "deferred");
	deferred["status"] =
		json!({"export":"ProbeState", "mode":"sync", "values":[{"value":1,"state":"active"}]});
	rt.plan(vec![rt.plugin("async_native", "native", "entry"), deferred]);
	let events = rt.run("delayed");
	let entry_active = events
		.iter()
		.position(|event| event["plugin_id"] == "async_native" && event["state"] == "active")
		.unwrap();
	let deferred_loading = events
		.iter()
		.position(|event| event["plugin_id"] == "sync" && event["state"] == "loading")
		.unwrap();
	assert!(entry_active < deferred_loading);
}

#[test]
fn invalid_status_is_terminal_and_never_repeatedly_polled() {
	let rt = Runtime::new();
	rt.plan(vec![
		rt.plugin("bad_status", "native", "entry"),
		rt.plugin("null_status", "native", "entry"),
	]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "bad_status").last(), Some(&"failed"));
	assert_eq!(states(&events, "null_status").last(), Some(&"refused"));
	assert!(
		!events
			.iter()
			.any(|v| v["message"] == "polled invalid status")
	);
	// The module cannot be unloaded while it may still have threads/hooks.
	assert!(states(&events, "bad_status").contains(&"dll_loaded"));
}

#[test]
fn hard_linked_entry_dlls_are_refused_with_or_without_a_digest() {
	use sha2::{Digest, Sha256};
	for with_digest in [false, true] {
		let rt = Runtime::new();
		let mut plugin = rt.plugin("native", "native", "entry");
		let entry = rt.path("plugins/native.dll");
		let outside = tempfile::tempdir().unwrap();
		let external = outside.path().join("external.dll");
		let bytes = fs::read(&entry).unwrap();
		fs::write(&external, &bytes).unwrap();
		fs::remove_file(&entry).unwrap();
		fs::hard_link(&external, &entry).unwrap();
		if with_digest {
			plugin["sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
		}
		rt.plan(vec![plugin]);
		let events = rt.run("plain");
		assert_eq!(states(&events, "native").last(), Some(&"refused"));
		assert!(!states(&events, "native").contains(&"dll_loaded"));
		assert!(events.iter().any(|event| {
			event["plugin_id"] == "native"
				&& event["reason"]
					.as_str()
					.is_some_and(|reason| reason.contains("single link"))
		}));
		assert_eq!(fs::read(external).unwrap(), bytes);
	}
}

#[test]
fn symbolic_linked_entry_dlls_are_refused_with_or_without_a_digest() {
	use sha2::{Digest, Sha256};
	for with_digest in [false, true] {
		let rt = Runtime::new();
		let mut plugin = rt.plugin("native", "native", "entry");
		let entry = rt.path("plugins/native.dll");
		let target = rt.path("plugins/selected-target.dll");
		let bytes = fs::read(&entry).unwrap();
		fs::rename(&entry, &target).unwrap();
		std::os::windows::fs::symlink_file(&target, &entry).unwrap();
		if with_digest {
			plugin["sha256"] = format!("{:x}", Sha256::digest(&bytes)).into();
		}
		rt.plan(vec![plugin]);
		let events = rt.run("plain");
		assert_eq!(states(&events, "native").last(), Some(&"refused"));
		assert!(!states(&events, "native").contains(&"dll_loaded"));
		assert!(events.iter().any(|event| {
			event["plugin_id"] == "native"
				&& event["reason"]
					.as_str()
					.is_some_and(|reason| reason.contains("ordinary file"))
		}));
		assert_eq!(fs::read(target).unwrap(), bytes);
	}
}

#[test]
fn a_hard_linked_plan_is_refused_before_loading_any_plugin() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("native", "native", "entry")]);
	let outside = tempfile::tempdir().unwrap();
	let bytes = fs::read(rt.path("foch-host/plan.json")).unwrap();
	let external = outside.path().join("plan.json");
	fs::write(&external, &bytes).unwrap();
	fs::remove_file(rt.path("foch-host/plan.json")).unwrap();
	fs::hard_link(&external, rt.path("foch-host/plan.json")).unwrap();
	rt.run("forward-only");
	assert!(!rt.path("foch-host/events.jsonl").exists());
	assert!(
		fs::read_to_string(rt.path("foch-host/host-error.txt"))
			.unwrap()
			.contains("single link")
	);
	assert_eq!(fs::read(external).unwrap(), bytes);
}

#[test]
fn frozen_digest_accepts_the_selected_bytes_and_external_paths_are_refused() {
	use sha2::{Digest, Sha256};
	let rt = Runtime::new();
	let mut native = rt.plugin("native", "native", "entry");
	native["sha256"] = format!(
		"{:x}",
		Sha256::digest(fs::read(rt.path("plugins/native.dll")).unwrap())
	)
	.into();
	let mut external = rt.plugin("sync", "legacy", "entry");
	external["path"] = fixtures().path().join("sync.dll").to_str().unwrap().into();
	rt.plan(vec![native, external]);
	let events = rt.run("native");
	assert_eq!(states(&events, "native").last(), Some(&"active"));
	assert_eq!(states(&events, "sync").last(), Some(&"refused"));
}

#[test]
fn an_unrelated_executable_uses_only_version_forwarding() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("native", "native", "entry")]);
	fs::rename(rt.path("eu4.exe"), rt.path("other.exe")).unwrap();
	let output = Command::new(rt.path("other.exe"))
		.arg("forward-only")
		.current_dir(rt.0.path())
		.output()
		.unwrap();
	assert!(output.status.success());
	assert!(!rt.path("foch-host/events.jsonl").exists());
}

#[test]
fn forwarding_only_initializes_safely_on_concurrent_first_calls() {
	let rt = Runtime::new();
	fs::rename(rt.path("eu4.exe"), rt.path("other.exe")).unwrap();
	let output = Command::new(rt.path("other.exe"))
		.arg("proxy-threads")
		.current_dir(rt.0.path())
		.output()
		.unwrap();
	assert!(
		output.status.success(),
		"{:?}: {}",
		output.status.code(),
		String::from_utf8_lossy(&output.stdout)
	);
	assert!(!rt.path("foch-host/events.jsonl").exists());
}

#[test]
fn refuses_a_native_callback_that_points_into_data() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("data_callback", "native", "entry")]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "data_callback").last(), Some(&"refused"));
}

#[test]
fn malformed_status_details_fail_and_stop_initialization_polling() {
	let rt = Runtime::new();
	let names = [
		"null_detail",
		"invalid_utf8",
		"unreadable_detail",
		"oversized_detail",
	];
	rt.plan(
		names
			.iter()
			.map(|id| rt.plugin(id, "native", "entry"))
			.collect(),
	);
	let events = rt.run("plain");
	for id in names {
		assert_eq!(states(&events, id).last(), Some(&"failed"), "{id}");
	}
	assert!(
		!events
			.iter()
			.any(|v| v["message"] == "polled invalid status")
	);
}

#[test]
fn reports_copy_unaligned_status_and_reject_unreadable_or_invalid_data() {
	let rt = Runtime::new();
	rt.plan(
		[
			"unaligned_report",
			"unreadable_report",
			"invalid_report_detail",
		]
		.into_iter()
		.map(|id| rt.plugin(id, "native", "entry"))
		.collect(),
	);
	let events = rt.run("plain");
	assert_eq!(states(&events, "unaligned_report").last(), Some(&"active"));
	for id in ["unreadable_report", "invalid_report_detail"] {
		assert_eq!(states(&events, id).last(), Some(&"failed"), "{id}");
	}
}

#[test]
fn a_short_report_reads_only_the_size_prefix_before_rejecting_it() {
	let rt = Runtime::new();
	rt.plan(vec![rt.plugin("short_report", "native", "entry")]);
	let events = rt.run("plain");
	assert_eq!(states(&events, "short_report").last(), Some(&"failed"));
	assert!(
		events
			.iter()
			.any(|event| event["plugin_id"] == "short_report"
				&& event["reason"] == "reported status structure is too small")
	);
}

#[test]
fn management_stages_a_wire_plan_that_the_real_host_executes() {
	use foch::plugin::{self, deployment, manifest::Manifest, store::ArchiveEntry};
	let source = tempfile::tempdir().unwrap();
	let output = tempfile::tempdir().unwrap();
	let store = output.path().join("store");
	fs::copy(
		fixtures().path().join("eu4.exe"),
		source.path().join("eu4.exe"),
	)
	.unwrap();
	fs::write(source.path().join("VERSION.dll"), "excluded old loader").unwrap();
	let mut selections = Vec::new();
	for (id, name, kind, phase) in [
		("z-native", "native", "native", "entry"),
		("a-legacy", "sync", "legacy", "deferred"),
	] {
		let status = if kind == "legacy" {
			"[status]\nexport = \"ProbeState\"\nmode = \"sync\"\nvalues = [{value=1,state=\"active\"}]\n"
		} else {
			"[config.greeting]\ntype=\"string\"\ndefault=\"hi\"\n"
		};
		let manifest = Manifest::parse(&format!("schema=1\n[plugin]\nid=\"{id}\"\nname=\"{id}\"\nversion=\"1.0.0\"\n[target]\ngame=\"eu4\"\nplatform=\"windows-x86_64\"\ngame_versions=\"=1.37.5\"\nabi_major=0\n[entry]\nkind=\"{kind}\"\nphase=\"{phase}\"\npath=\"plugins/{name}.dll\"\n{status}")).unwrap();
		let package = plugin::store::adapt(
			manifest,
			vec![ArchiveEntry {
				path: format!("plugins/{name}.dll"),
				data: fs::read(fixtures().path().join(format!("{name}.dll"))).unwrap(),
			}],
		)
		.unwrap();
		plugin::install(&store, &package).unwrap();
		selections.push(plugin::Selection {
			id: id.into(),
			version: "1.0.0".parse().unwrap(),
			enabled: true,
			config: json!({}),
		});
	}
	selections.push(plugin::Selection {
		id: "disabled".into(),
		version: "1.0.0".parse().unwrap(),
		enabled: false,
		config: json!({}),
	});
	let game = plugin::GameIdentity {
		game: "eu4".into(),
		version: "1.37.5".parse().unwrap(),
		platform: plugin::WINDOWS_X64.into(),
	};
	// Input hard links remain read-only inputs: resolution freezes bytes,
	// staging copies them into independent files, and later drift is refused.
	let (versions, _) = plugin::store::installed_versions(&store);
	let installed = versions
		.iter()
		.find(|item| item.manifest.plugin.id == "z-native")
		.unwrap();
	let native_source = installed.dir.join(&installed.manifest.entry.path);
	let linked_source = output.path().join("externally-linked-native.dll");
	fs::copy(&native_source, &linked_source).unwrap();
	fs::remove_file(&native_source).unwrap();
	fs::hard_link(&linked_source, &native_source).unwrap();
	let selected = deployment::resolve(&game, &store, &selections).unwrap();
	fs::write(&linked_source, "changed").unwrap();
	assert!(deployment::resolve(&game, &store, &selections).is_err());
	let layer = foch_runtime::runner::runtime::RuntimeLayer::prepare(
		source.path(),
		&output.path().join("rt"),
		"wire-run",
	)
	.unwrap();
	let dll = std::env::current_exe()
		.unwrap()
		.parent()
		.unwrap()
		.join("foch_runtime_host.dll");
	fs::copy(dll, layer.directory.join("VERSION.dll")).unwrap();
	let prepared = selected
		.stage(
			&layer.directory,
			&output.path().join("data"),
			"wire-run",
			source.path(),
		)
		.unwrap();
	let wire = serde_json::to_string(&prepared.plan).unwrap();
	prepared.plan.validate().unwrap();
	let parsed = foch_runtime::plan::parse(&wire).unwrap();
	assert_eq!(
		parsed
			.plugins
			.iter()
			.map(|plugin| plugin.id.as_str())
			.collect::<Vec<_>>(),
		["z-native", "a-legacy"]
	);
	let events = run_runtime(&layer.directory, "native");
	assert_eq!(states(&events, "z-native").last(), Some(&"active"));
	assert_eq!(states(&events, "a-legacy").last(), Some(&"active"));
	let actual = deployment::states(&prepared.plan, &layer.directory).unwrap();
	assert_eq!(actual["z-native"]["state"], "active");
	assert!(!actual.contains_key("disabled"));
	// Keep management's preflight aligned with the host's actual wire parser.
	for change in 0..8 {
		let mut invalid = serde_json::to_value(&prepared.plan).unwrap();
		match change {
			0 => invalid["plugins"][1]["path"] = invalid["plugins"][0]["path"].clone(),
			1 => invalid["plugins"][1]["status"]["mode"] = "poll".into(),
			2 => invalid["plugins"][1]["status"]["values"][0]["state"] = "healthy".into(),
			3 => invalid["plugins"][0]["abi_major"] = 9.into(),
			4 => invalid["plugins"][0]["config_json"] = "[]".into(),
			5 => invalid["plugins"][0]["dirs"]["plugin"] = "relative".into(),
			6 => invalid["plugins"][0]["path"] = "C:\\rt\\..\\bad.dll".into(),
			_ => invalid["plugins"].as_array_mut().unwrap().reverse(),
		}
		let managed: deployment::HostPlan = serde_json::from_value(invalid.clone()).unwrap();
		assert!(
			managed.validate().is_err(),
			"management accepted case {change}"
		);
		assert!(
			foch_runtime::plan::parse(&invalid.to_string()).is_err(),
			"host accepted case {change}"
		);
	}
	assert_eq!(fs::read(linked_source).unwrap(), b"changed");
}
