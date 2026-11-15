/* A macOS test lever for tests/rex_install_live.py: it holds a Namedrop
 * process at one blocking flock(2) so the live harness can stop or resume it
 * at a point UI.md names, such as a removal after its marker and before its
 * shared gate. Loaded only into the process under test, through
 * DYLD_INSERT_LIBRARIES, with ND_TEST_PAUSE=<path suffix>:<SH|EX>:<pause file>.
 * While the pause file exists, a blocking flock of that kind on a descriptor
 * whose path ends with the suffix waits, after creating <pause file>.arrived.
 * Non-blocking attempts and every other lock pass straight through. */
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/param.h>
#include <unistd.h>

#define DYLD_INTERPOSE(replacement, replacee)                                  \
    __attribute__((used)) static struct {                                      \
        const void *replacement;                                               \
        const void *replacee;                                                  \
    } interpose_##replacee __attribute__((section("__DATA,__interpose"))) = {  \
        (const void *)(unsigned long)&replacement,                             \
        (const void *)(unsigned long)&replacee};

static int paused_flock(int fd, int operation) {
    const char *spec = getenv("ND_TEST_PAUSE");
    if (spec != NULL && (operation & LOCK_NB) == 0) {
        char copy[MAXPATHLEN * 2];
        snprintf(copy, sizeof copy, "%s", spec);
        char *suffix = copy;
        char *kind = strchr(suffix, ':');
        char *pause = kind ? strchr(kind + 1, ':') : NULL;
        if (kind != NULL && pause != NULL) {
            *kind++ = '\0';
            *pause++ = '\0';
            int wanted = strcmp(kind, "SH") == 0 ? LOCK_SH : LOCK_EX;
            char path[MAXPATHLEN];
            size_t length = strlen(suffix);
            if ((operation & (LOCK_SH | LOCK_EX)) == wanted &&
                fcntl(fd, F_GETPATH, path) == 0 && strlen(path) >= length &&
                strcmp(path + strlen(path) - length, suffix) == 0 &&
                access(pause, F_OK) == 0) {
                char arrived[MAXPATHLEN + 16];
                snprintf(arrived, sizeof arrived, "%s.arrived", pause);
                int marker = open(arrived, O_CREAT | O_WRONLY, 0600);
                if (marker >= 0) {
                    close(marker);
                }
                while (access(pause, F_OK) == 0) {
                    usleep(10000);
                }
            }
        }
    }
    return flock(fd, operation);
}

DYLD_INTERPOSE(paused_flock, flock)
