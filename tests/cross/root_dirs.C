// Directories, written and updated by ROOT itself (a fixture, and the
// reference for how rhizoid writes subdirectories and updates files).
//
//   root -b -q -l 'tests/cross/root_dirs.C("root_dirs.root")'
//
// writes `n` (a TNamed) at the top, `a/n`, `a/b/n` and an empty `c`.
//
//   root -b -q -l 'tests/cross/root_dirs.C("file.root", "update")'
//
// opens a file in UPDATE mode and adds `a/b/m` and `d/m` (ROOT's own update,
// run on files rhizoid updated).
//
//   root -b -q -l 'tests/cross/root_dirs.C("file.root", "dump")'
//
// prints every directory and key, recursively (`DIR path` and
// `KEY path/name;cycle class title`), and every TNamed's title
// (`NAMED path title`), as ROOT reads them.

#include <TDirectory.h>
#include <TFile.h>
#include <TKey.h>
#include <TNamed.h>
#include <TSystem.h>

#include <cstdio>
#include <string>

static void walk(TDirectory *d, const std::string &path)
{
   printf("DIR %s\n", path.c_str());
   for (auto k : *d->GetListOfKeys()) {
      auto key = static_cast<TKey *>(k);
      std::string full = path.empty() ? key->GetName() : path + "/" + key->GetName();
      printf("KEY %s;%d %s %s\n", full.c_str(), key->GetCycle(), key->GetClassName(), key->GetTitle());
      if (std::string(key->GetClassName()) == "TDirectoryFile" || std::string(key->GetClassName()) == "TDirectory") {
         walk(static_cast<TDirectory *>(key->ReadObj()), full);
      } else if (std::string(key->GetClassName()) == "TNamed") {
         auto n = static_cast<TNamed *>(key->ReadObj());
         printf("NAMED %s;%d %s\n", full.c_str(), key->GetCycle(), n->GetTitle());
      }
   }
}

void root_dirs(const char *path, const char *action = nullptr)
{
   std::string what = action ? action : "";
   if (what == "dump") {
      TFile *f = TFile::Open(path);
      if (!f || f->IsZombie()) {
         printf("FILE error cannot-open\n");
         gSystem->Exit(1);
      }
      printf("FILE recovered %d\n", f->TestBit(TFile::kRecovered) ? 1 : 0);
      walk(f, "");
      return;
   }
   if (what == "update") {
      TFile f(path, "UPDATE");
      f.cd("a/b");
      TNamed("m", "added by ROOT in a/b").Write();
      f.mkdir("d")->cd();
      TNamed("m", "added by ROOT in d").Write();
      f.Close();
      return;
   }
   TFile f(path, "RECREATE");
   TNamed("n", "top").Write();
   auto a = f.mkdir("a", "directory a");
   a->cd();
   TNamed("n", "in a").Write();
   auto b = a->mkdir("b", "directory b");
   b->cd();
   TNamed("n", "in a/b").Write();
   f.mkdir("c");
   f.Close();
}
