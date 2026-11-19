#define _POSIX_C_SOURCE 200809L
#if defined(__APPLE__)
#define _DARWIN_C_SOURCE
#endif
#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <unistd.h>

#if defined(__APPLE__)
#include <libproc.h>
#include <mach/mach.h>
#include <sys/sysctl.h>
#elif !defined(__linux__)
#error unsupported measurement platform
#endif

#define MAX_PROCESSES 256

#if defined(__linux__)
static int start_identity(int pid, uint64_t *identity) {
    char filename[64];
    char buffer[4096];
    snprintf(filename, sizeof(filename), "/proc/%d/stat", pid);
    FILE *input = fopen(filename, "r");
    if (!input) return errno;
    char *line = fgets(buffer, sizeof(buffer), input);
    int error = ferror(input) ? EIO : 0;
    fclose(input);
    if (!line || error) return error ? error : EIO;
    char *cursor = strrchr(buffer, ')');
    if (!cursor || cursor[1] != ' ') return EINVAL;
    cursor += 2;
    for (int field = 3; field < 22; field++) {
        cursor = strchr(cursor, ' ');
        if (!cursor) return EINVAL;
        cursor++;
    }
    errno = 0;
    char *end;
    uintmax_t value = strtoumax(cursor, &end, 10);
    if (errno || end == cursor || *end != ' ' || value > UINT64_MAX) return EINVAL;
    *identity = (uint64_t)value;
    return 0;
}

static int process_values(int pid, uint64_t *identity, uint64_t values[8]) {
    const char *keys[] = {"Rss:", "Pss:", "Private_Clean:", "Private_Dirty:",
                          "Shared_Clean:", "Shared_Dirty:", "Swap:", "SwapPss:"};
    int error = start_identity(pid, identity);
    if (error) return error;
    char filename[64];
    snprintf(filename, sizeof(filename), "/proc/%d/smaps_rollup", pid);
    FILE *input = fopen(filename, "r");
    if (!input) return errno;
    char line[512];
    unsigned found = 0;
    while (fgets(line, sizeof(line), input)) {
        for (size_t index = 0; index < 8; index++) {
            size_t length = strlen(keys[index]);
            if (strncmp(line, keys[index], length)) continue;
            uintmax_t value;
            char unit[3];
            char extra;
            if ((found & (1u << index)) ||
                sscanf(line + length, " %ju %2s %c", &value, unit, &extra) != 2 ||
                strcmp(unit, "kB") || value > UINT64_MAX / 1024) {
                error = EINVAL;
                break;
            }
            values[index] = (uint64_t)value * 1024;
            found |= 1u << index;
        }
        if (error) break;
    }
    if (!error && ferror(input)) error = EIO;
    fclose(input);
    if (!error && found != 255) error = ENODATA;
    uint64_t after = 0;
    if (!error) error = start_identity(pid, &after);
    if (!error && after != *identity) error = ESTALE;
    return error;
}

static void host_values(void) {
    FILE *input = fopen("/proc/meminfo", "r");
    if (!input) {
        printf("{\"status\":\"error\",\"errno\":%d}", errno);
        return;
    }
    uint64_t total = 0;
    uint64_t available = 0;
    unsigned found = 0;
    int error = 0;
    char line[512];
    while (fgets(line, sizeof(line), input)) {
        uintmax_t value;
        if (sscanf(line, "SwapTotal: %ju kB", &value) == 1) {
            if (value > UINT64_MAX / 1024 || (found & 1)) error = EINVAL;
            else { total = (uint64_t)value * 1024; found |= 1; }
        } else if (sscanf(line, "SwapFree: %ju kB", &value) == 1) {
            if (value > UINT64_MAX / 1024 || (found & 2)) error = EINVAL;
            else { available = (uint64_t)value * 1024; found |= 2; }
        }
    }
    if (ferror(input)) error = EIO;
    fclose(input);
    if (found != 3 || available > total) error = EINVAL;
    if (error) printf("{\"status\":\"error\",\"errno\":%d}", error);
    else printf("{\"status\":\"ok\",\"swap_total_bytes\":%" PRIu64
                ",\"swap_used_bytes\":%" PRIu64 "}", total, total - available);
}
#else
static void host_values(void) {
    struct xsw_usage swap;
    size_t size = sizeof(swap);
    if (sysctlbyname("vm.swapusage", &swap, &size, NULL, 0) || size != sizeof(swap)) {
        printf("{\"status\":\"error\",\"errno\":%d}", errno ? errno : EINVAL);
        return;
    }
    vm_statistics64_data_t statistics = {0};
    mach_msg_type_number_t count = HOST_VM_INFO64_COUNT;
    mach_port_t host = mach_host_self();
    kern_return_t status = host_statistics64(host, HOST_VM_INFO64, (host_info64_t)&statistics, &count);
    mach_port_deallocate(mach_task_self(), host);
    long page_bytes = sysconf(_SC_PAGESIZE);
    if (status != KERN_SUCCESS || count < HOST_VM_INFO64_REV1_COUNT || page_bytes <= 0) {
        if (status != KERN_SUCCESS) printf("{\"status\":\"error\",\"mach_status\":%d}", status);
        else printf("{\"status\":\"error\",\"errno\":%d}", EINVAL);
        return;
    }
    printf("{\"status\":\"ok\",\"swap_total_bytes\":%" PRIu64
           ",\"swap_used_bytes\":%" PRIu64 ",\"compressor_resident_bytes\":%" PRIu64
           ",\"compressed_logical_bytes\":%" PRIu64 ",\"compressor_swapped_page_bytes\":",
           swap.xsu_total, swap.xsu_used,
           (uint64_t)statistics.compressor_page_count * (uint64_t)page_bytes,
           statistics.total_uncompressed_pages_in_compressor * (uint64_t)page_bytes);
#ifdef HOST_VM_INFO64_REV2_COUNT
    if (count >= HOST_VM_INFO64_REV2_COUNT) {
        printf("%" PRIu64, statistics.swapped_count * (uint64_t)page_bytes);
    } else {
        printf("null");
    }
#else
    printf("null");
#endif
    printf("}");
}
#endif

int main(int argc, char **argv) {
    if (argc < 2 || argc > MAX_PROCESSES + 1) return 2;
    int pids[MAX_PROCESSES];
    for (int index = 1; index < argc; index++) {
        const char *text = argv[index];
        if (!*text || strspn(text, "0123456789") != strlen(text)) return 2;
        errno = 0;
        char *end;
        long value = strtol(text, &end, 10);
        if (errno || *end || value <= 0 || value > INT_MAX) return 2;
        for (int previous = 0; previous < index - 1; previous++) {
            if (pids[previous] == value) return 2;
        }
        pids[index - 1] = (int)value;
    }
#if defined(__APPLE__)
    printf("{\"version\":1,\"platform\":\"Darwin\",\"processes\":[");
#else
    printf("{\"version\":1,\"platform\":\"Linux\",\"processes\":[");
#endif
    for (int index = 0; index < argc - 1; index++) {
        if (index) putchar(',');
        int pid = pids[index];
        uint64_t identity = 0;
        uint64_t values[8] = {0};
#if defined(__APPLE__)
        struct rusage_info_v0 usage;
        int error = proc_pid_rusage(pid, RUSAGE_INFO_V0, (rusage_info_t *)&usage) ? errno : 0;
        if (!error && usage.ri_proc_exit_abstime != 0) error = ESRCH;
        if (!error) {
            identity = usage.ri_proc_start_abstime;
            values[0] = usage.ri_resident_size;
            values[1] = usage.ri_phys_footprint;
        }
#else
        int error = process_values(pid, &identity, values);
#endif
        printf("{\"pid\":%d,", pid);
        if (error) {
            printf("\"status\":\"error\",\"errno\":%d}", error);
            continue;
        }
        printf("\"status\":\"ok\",\"start_id\":\"%" PRIu64 "\",\"rss_bytes\":%" PRIu64,
               identity, values[0]);
#if defined(__APPLE__)
        printf(",\"physical_footprint_bytes\":%" PRIu64 "}", values[1]);
#else
        const char *fields[] = {"pss_bytes", "private_clean_bytes", "private_dirty_bytes",
                                "shared_clean_bytes", "shared_dirty_bytes", "swap_bytes", "swap_pss_bytes"};
        for (size_t field = 0; field < 7; field++) printf(",\"%s\":%" PRIu64, fields[field], values[field + 1]);
        putchar('}');
#endif
    }
    printf("],\"host\":");
    host_values();
    struct rusage helper;
    if (getrusage(RUSAGE_SELF, &helper)) return 3;
    printf(",\"helper\":{\"pid\":%d,\"user_cpu_us\":%" PRIu64
           ",\"system_cpu_us\":%" PRIu64 ",\"max_rss_bytes\":%" PRIu64 "}",
           getpid(), (uint64_t)helper.ru_utime.tv_sec * 1000000 + (uint64_t)helper.ru_utime.tv_usec,
           (uint64_t)helper.ru_stime.tv_sec * 1000000 + (uint64_t)helper.ru_stime.tv_usec,
#if defined(__APPLE__)
           (uint64_t)helper.ru_maxrss);
#else
           (uint64_t)helper.ru_maxrss * 1024);
#endif
    puts("}");
    return ferror(stdout) ? 3 : 0;
}
