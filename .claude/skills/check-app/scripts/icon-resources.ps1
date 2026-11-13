# Lists a program's (or DLL's) icon group as its directory lists it (size, colour count, bit
# count, resource id) beside what each image's own header says (PNG: bit depth and colour type;
# bitmap: bits a pixel). The program is mapped as data: none of its code runs.
#
#   .\icon-resources.ps1 -File 'C:\...\game.exe' [-Index 0]
#
# -Index picks the group as an IconRef does: 0 and up counts groups in resource order, below 0 is
# the group whose resource id is its opposite. Every group's id is printed first.
param([Parameter(Mandatory)][string]$File, [int]$Index = 0)

if (-not ('GmNative.Resources' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
namespace GmNative {
public static class Resources {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryExW(string name, IntPtr file, uint flags);
    [DllImport("kernel32.dll")] public static extern bool FreeLibrary(IntPtr module);
    [DllImport("kernel32.dll")] static extern IntPtr FindResourceW(IntPtr module, IntPtr name, IntPtr type);
    [DllImport("kernel32.dll")] static extern IntPtr LoadResource(IntPtr module, IntPtr info);
    [DllImport("kernel32.dll")] static extern IntPtr LockResource(IntPtr data);
    [DllImport("kernel32.dll")] static extern uint SizeofResource(IntPtr module, IntPtr info);
    delegate bool EnumName(IntPtr module, IntPtr type, IntPtr name, IntPtr param);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    static extern bool EnumResourceNamesW(IntPtr module, IntPtr type, EnumName callback, IntPtr param);

    // Group names in resource order: "#<id>" for numbers, else the name.
    public static List<string> Names(IntPtr module, int type) {
        var names = new List<string>();
        EnumResourceNamesW(module, (IntPtr)type, (m, t, name, p) => {
            long value = name.ToInt64();
            names.Add((value >> 16) == 0 ? "#" + value : Marshal.PtrToStringUni(name));
            return true;
        }, IntPtr.Zero);
        return names;
    }

    public static byte[] Read(IntPtr module, string name, int type) {
        IntPtr namePtr = name.StartsWith("#") ? (IntPtr)int.Parse(name.Substring(1)) : Marshal.StringToHGlobalUni(name);
        IntPtr info = FindResourceW(module, namePtr, (IntPtr)type);
        if (info == IntPtr.Zero) return null;
        uint size = SizeofResource(module, info);
        IntPtr data = LockResource(LoadResource(module, info));
        var bytes = new byte[size];
        Marshal.Copy(data, bytes, 0, (int)size);
        return bytes;
    }
}
}
'@
}

# LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE
$module = [GmNative.Resources]::LoadLibraryExW($File, [System.IntPtr]::Zero, 0x22)
if ($module -eq [System.IntPtr]::Zero) { throw "Can't map $File" }
try {
    $groups = [GmNative.Resources]::Names($module, 14)
    $more = if ($groups.Count -gt 20) { ", ... ($($groups.Count) in all)" } else { '' }
    "icon groups: $(($groups | Select-Object -First 20) -join ', ')$more"
    $name = if ($Index -ge 0) { $groups[$Index] } else { "#$(-$Index)" }
    if (-not $name) { throw "No icon group at index $Index" }
    $directory = [GmNative.Resources]::Read($module, $name, 14)
    $count = [BitConverter]::ToUInt16($directory, 4)
    "group $name : $count images"
    for ($i = 0; $i -lt $count; $i++) {
        $entry = 6 + 14 * $i
        $side = $directory[$entry]; if ($side -eq 0) { $side = 256 }
        $colours = $directory[$entry + 2]
        $bits = [BitConverter]::ToUInt16($directory, $entry + 6)
        $id = [BitConverter]::ToUInt16($directory, $entry + 12)
        $image = [GmNative.Resources]::Read($module, "#$id", 3)
        if ($image[0] -eq 0x89 -and $image[1] -eq 0x50) {
            $own = 'PNG depth {0}, colour type {1}' -f $image[24], $image[25]
        } else {
            $own = 'bitmap {0} bits' -f [BitConverter]::ToUInt16($image, 14)
        }
        'id {0,4}: listed {1,3} px, {2,3} colours, {3,2} bits | {4} ({5} bytes)' -f $id, $side, $colours, $bits, $own, $image.Length
    }
} finally {
    [void][GmNative.Resources]::FreeLibrary($module)
}
