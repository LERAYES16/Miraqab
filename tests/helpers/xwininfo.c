/*
 * Minimal stand-in for xwininfo(1), supporting only "-id 0xID", used by
 * Miraqab's end-to-end tests when the real x11-utils package is not installed
 * (NOT shipped to users).
 *
 * It prints the same "key: value" lines the real tool prints for the fields
 * Miraqab reads.  The format is reproduced from the documented output of the
 * real xwininfo; it has not been diffed against a real xwininfo here.
 */
#include <X11/Xlib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int had_error = 0;
static int on_error(Display *d, XErrorEvent *e) {
    (void)d; (void)e;
    had_error = 1;
    return 0;
}

int main(int argc, char **argv) {
    unsigned long id = 0;
    for (int i = 1; i < argc - 1; i++) {
        if (strcmp(argv[i], "-id") == 0) id = strtoul(argv[i + 1], NULL, 0);
    }
    if (!id) { fprintf(stderr, "xwininfo: usage: xwininfo -id ID\n"); return 1; }

    Display *dpy = XOpenDisplay(NULL);
    if (!dpy) { fprintf(stderr, "xwininfo: unable to open display\n"); return 1; }
    XSetErrorHandler(on_error);

    XWindowAttributes a;
    Status ok = XGetWindowAttributes(dpy, (Window)id, &a);
    XSync(dpy, False);
    if (!ok || had_error) {
        fprintf(stderr, "xwininfo: Error: No such window with id 0x%lx.\n", id);
        return 1;
    }
    Window root = DefaultRootWindow(dpy), child;
    int ax = 0, ay = 0;
    XTranslateCoordinates(dpy, (Window)id, root, 0, 0, &ax, &ay, &child);

    const char *ms = a.map_state == IsViewable ? "IsViewable"
                   : a.map_state == IsUnviewable ? "IsUnviewable" : "IsUnMapped";
    printf("\nxwininfo: Window id: 0x%lx \"Miraqab Test Window\"\n\n", id);
    printf("  Absolute upper-left X:  %d\n", ax);
    printf("  Absolute upper-left Y:  %d\n", ay);
    printf("  Relative upper-left X:  %d\n", a.x);
    printf("  Relative upper-left Y:  %d\n", a.y);
    printf("  Width: %d\n", a.width);
    printf("  Height: %d\n", a.height);
    printf("  Depth: %d\n", a.depth);
    printf("  Border width: %d\n", a.border_width);
    printf("  Map State: %s\n", ms);
    XCloseDisplay(dpy);
    return 0;
}
