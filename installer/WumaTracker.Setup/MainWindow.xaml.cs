using System.IO;
using System.Windows;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace WumaTracker.Setup;

public partial class MainWindow : Window
{
    private readonly SetupMode _mode;
    private readonly bool _simulateError;
    private readonly bool _unattended;
    private readonly bool _restartAfterInstall;
    private readonly string _relaunchArgs;
    private volatile bool _finished;
    private volatile bool _succeeded;
    private string _fullStatusText = "";

    public MainWindow(SetupMode mode, bool simulateError = false, bool unattended = false,
        bool restartAfterInstall = false, string relaunchArgs = "")
    {
        _mode = mode;
        _simulateError = simulateError;
        _unattended = unattended;
        _restartAfterInstall = restartAfterInstall;
        _relaunchArgs = relaunchArgs;
        InitializeComponent();
        Title = mode == SetupMode.Uninstall ? "트래커 제거" : "트래커 설치";
        StatusText.Text = mode == SetupMode.Uninstall ? "제거 중..." : "설치 중...";
        ApplyTheme(ThemeHelper.IsDarkMode());
        Loaded += MainWindow_Loaded;
    }

    private void ApplyTheme(bool dark)
    {
        // No resource-dictionary theme switching yet — first pass just sets
        // the handful of colors that actually differ between the two.
        var background = dark ? Color.FromRgb(0x20, 0x20, 0x22) : Colors.White;
        var foreground = dark ? Colors.White : Color.FromRgb(0x1A, 0x1A, 0x1A);
        var border = dark ? Color.FromRgb(0x3A, 0x3A, 0x3D) : Color.FromRgb(0xE4, 0xE4, 0xE7);
        var progressTrack = dark ? Color.FromRgb(0x3A, 0x3A, 0x3D) : Color.FromRgb(0xE9, 0xE9, 0xEC);
        // Light-mode brand blue (#2A408D) reads as muted/dead against a dark
        // background — same hue/saturation, lightness raised (HSL 36% -> 60%)
        // for a dark-mode primary that actually has contrast.
        var primary = dark ? Color.FromRgb(0x5F, 0x79, 0xD3) : Color.FromRgb(0x2A, 0x40, 0x8D);

        RootBorder.Background = new SolidColorBrush(background);
        RootBorder.BorderBrush = new SolidColorBrush(border);
        StatusText.Foreground = new SolidColorBrush(foreground);
        CloseButton.Foreground = new SolidColorBrush(foreground);
        Progress.Background = new SolidColorBrush(progressTrack);
        Progress.Foreground = new SolidColorBrush(primary);

        FinishCloseButton.Foreground = new SolidColorBrush(foreground);
        FinishCloseButton.Background = new SolidColorBrush(progressTrack);
        FinishCloseButton.BorderBrush = new SolidColorBrush(border);
        LaunchAppCheckBox.Foreground = new SolidColorBrush(foreground);
        CopyStatusButton.Foreground = new SolidColorBrush(primary);

        // logo-dark.png is white-on-transparent (only visible against a
        // dark page background); logo-light.png is the brand-blue version.
        LogoImage.Source = new BitmapImage(new Uri(
            dark ? "pack://application:,,,/Assets/logo-dark.png"
                 : "pack://application:,,,/Assets/logo-light.png"));
    }

    private void TitleBar_MouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (e.ButtonState == MouseButtonState.Pressed)
        {
            DragMove();
        }
    }

    private async void MainWindow_Loaded(object sender, RoutedEventArgs e)
    {
        var progress = new Progress<InstallProgress>(p =>
        {
            Progress.Value = p.Percent;
            _fullStatusText = p.Message;
            StatusText.Text = p.Message;
        });

        try
        {
            if (_simulateError)
            {
                await Task.Delay(400);
                throw new InvalidOperationException(
                    "이건 --simulate-error로 만든 테스트용 오류 메시지입니다. 실제 설치 실패 시 나오는 " +
                    "예외 메시지가 얼마나 길어질 수 있는지, 2줄을 넘어가면 말줄임(...) 처리가 잘 되는지, " +
                    "그리고 \"전체 텍스트 복사\" 버튼으로 이 문장 전체와 스택 트레이스까지 클립보드에 " +
                    "제대로 복사되는지 확인하기 위한 충분히 긴 더미 텍스트입니다.",
                    new FileNotFoundException("(테스트) 더미 내부 예외: payload\\example.file"));
            }
            if (_mode == SetupMode.Uninstall)
            {
                await Task.Run(() => Installer.Uninstall(progress));
            }
            else
            {
                await Task.Run(() => Installer.Run(progress));
            }
            _succeeded = true;
        }
        catch (Exception ex)
        {
            // The full exception (not just Message) is what's actually
            // useful to whoever ends up debugging a report of this — the
            // on-screen label only has room for a couple of wrapped lines
            // (TextTrimming below), so "복사" is the only way to get the
            // rest out.
            _fullStatusText = $"오류가 발생했습니다:\n{ex}";
            StatusText.Text = $"오류가 발생했습니다: {ex.Message}";
            CopyStatusButton.Visibility = Visibility.Visible;
        }
        finally
        {
            _finished = true;
            Progress.Value = 100;
            FinishCloseButton.Visibility = Visibility.Visible;
            // No point offering to launch an app that either isn't there
            // (install failed) or was just removed (uninstall).
            if (_mode == SetupMode.Install && _succeeded && !_unattended)
            {
                LaunchAppCheckBox.Visibility = Visibility.Visible;
            }
        }

        // Passive-mode auto-update (tauri-plugin-updater's /P /UPDATE /R):
        // nobody is watching this window to click anything, so launch the
        // app (if requested) and close on our own — briefly, so a
        // passive-mode install still visibly finishes instead of just
        // vanishing, but without waiting on a click that will never come.
        if (_unattended)
        {
            if (_mode == SetupMode.Install && _succeeded && _restartAfterInstall)
            {
                Installer.LaunchApp(_relaunchArgs);
            }
            await Task.Delay(700);
            Close();
        }
    }

    private void CloseButton_Click(object sender, RoutedEventArgs e)
    {
        if (_finished)
        {
            Close();
            return;
        }

        if (_unattended)
        {
            // Passive mode is "progress bar, no interaction expected" —
            // nobody to confirm a cancel with.
            Environment.Exit(1);
            return;
        }

        var result = MessageBox.Show(
            this,
            _mode == SetupMode.Uninstall ? "제거를 취소하시겠습니까?" : "설치를 취소하시겠습니까?",
            Title,
            MessageBoxButton.YesNo,
            MessageBoxImage.Warning,
            MessageBoxResult.No);

        if (result == MessageBoxResult.Yes)
        {
            Environment.Exit(1);
        }
    }

    private void CopyStatusButton_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            Clipboard.SetText(_fullStatusText);
        }
        catch
        {
            // clipboard access can transiently fail if another process has
            // it open — not worth surfacing an error over
        }
    }

    private void FinishCloseButton_Click(object sender, RoutedEventArgs e)
    {
        if (_mode == SetupMode.Install && _succeeded && !_unattended && LaunchAppCheckBox.IsChecked == true)
        {
            try
            {
                Installer.LaunchApp();
            }
            catch
            {
                // best effort — a failed auto-launch shouldn't block closing
            }
        }

        Close();
    }
}
