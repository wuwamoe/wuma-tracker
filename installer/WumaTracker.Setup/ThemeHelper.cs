using Microsoft.Win32;

namespace WumaTracker.Setup;

public static class ThemeHelper
{
    public static bool IsDarkMode()
    {
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            var value = key?.GetValue("AppsUseLightTheme");
            return value is int lightTheme && lightTheme == 0;
        }
        catch
        {
            // Missing on older Windows builds (no dark mode support there) —
            // default to light, matching the OS's own default in that case.
            return false;
        }
    }
}
