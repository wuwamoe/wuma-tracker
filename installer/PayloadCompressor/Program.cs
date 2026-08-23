using System.IO.Compression;

if (args.Length != 2)
{
    Console.Error.WriteLine("Usage: PayloadCompressor <input> <output>");
    return 1;
}

using var input = File.OpenRead(args[0]);
using var output = File.Create(args[1]);
using var brotli = new BrotliSharpLib.BrotliStream(output, CompressionMode.Compress);
// Quality 11 (max) — this runs once at build time, not something an end
// user waits on, so there's no reason to trade ratio for speed here.
brotli.SetQuality(11);
input.CopyTo(brotli);
return 0;
