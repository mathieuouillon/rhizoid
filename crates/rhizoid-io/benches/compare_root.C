// ROOT's side of `benches/compare.py`: the same operations as the rhizoid
// benchmarks, on the same files, best of `repeat` runs. Prints
// `BENCH name seconds`.
//
//   root -b -q -l 'benches/compare_root.C("/tmp/rhizoid-bench", 5)'

#include <ROOT/RNTupleReader.hxx>
#include <TFile.h>
#include <TROOT.h>
#include <TStopwatch.h>
#include <TTree.h>
#include <TTreeReader.h>
#include <TTreeReaderArray.h>
#include <TTreeReaderValue.h>

#include <algorithm>
#include <cstdio>
#include <functional>
#include <string>
#include <vector>

#if ROOT_VERSION_CODE < ROOT_VERSION(6, 36, 0)
namespace RNT = ROOT::Experimental;
#else
namespace RNT = ROOT;
#endif

static void bench(const char *name, int repeat, const std::function<void()> &f)
{
   double best = 1e30;
   for (int i = 0; i < repeat; ++i) {
      TStopwatch w;
      w.Start();
      f();
      w.Stop();
      best = std::min(best, w.RealTime());
   }
   std::printf("BENCH %s %.6f\n", name, best);
}

static void read_tree(const std::string &path, const char *branch)
{
   auto f = std::unique_ptr<TFile>(TFile::Open(path.c_str()));
   TTreeReader r("events", f.get());
   TTreeReaderValue<float> x(r, branch);
   std::vector<float> out;
   out.reserve(1000000);
   while (r.Next())
      out.push_back(*x);
}

void compare_root(const char *dir, int repeat = 5)
{
   std::string d = dir;
   for (auto name : {"tree_zlib", "tree_lz4", "rntuple"}) {
      std::string path = d + "/" + name + ".root";
      bench((std::string("open/") + name).c_str(), repeat, [&] {
         auto f = std::unique_ptr<TFile>(TFile::Open(path.c_str()));
         f->GetListOfKeys()->GetEntries();
      });
   }
   for (auto name : {"tree_zlib", "tree_lz4"}) {
      std::string path = d + "/" + name + ".root";
      bench((std::string("read/f32/") + name).c_str(), repeat, [&] { read_tree(path, "x"); });
      bench((std::string("read/jagged_f32/") + name).c_str(), repeat, [&] {
         auto f = std::unique_ptr<TFile>(TFile::Open(path.c_str()));
         TTreeReader r("events", f.get());
         TTreeReaderArray<float> pt(r, "pt");
         std::vector<float> values;
         std::vector<std::size_t> offsets{0};
         while (r.Next()) {
            for (float v : pt)
               values.push_back(v);
            offsets.push_back(values.size());
         }
      });
   }
   std::string nt = d + "/rntuple.root";
   bench("read/f32/rntuple", repeat, [&] {
      auto reader = RNT::RNTupleReader::Open("events", nt);
      auto x = reader->GetView<float>("x");
      std::vector<float> out;
      out.reserve(reader->GetNEntries());
      for (auto i : reader->GetEntryRange())
         out.push_back(x(i));
   });
   bench("read/jagged_f32/rntuple", repeat, [&] {
      auto reader = RNT::RNTupleReader::Open("events", nt);
      auto pt = reader->GetView<std::vector<float>>("pt");
      std::vector<float> values;
      for (auto i : reader->GetEntryRange()) {
         const auto &v = pt(i);
         values.insert(values.end(), v.begin(), v.end());
      }
   });
   for (auto [name, algo] : {std::pair{"zlib", 101}, std::pair{"lz4", 404}}) {
      bench((std::string("write/tree/") + name).c_str(), repeat, [&] {
         TFile f("/tmp/rhizoid-bench-root-write.root", "RECREATE", "", algo);
         TTree t("events", "");
         Int_t id, n;
         Float_t x, pt[16];
         t.Branch("id", &id, "id/I");
         t.Branch("x", &x, "x/F");
         t.Branch("n", &n, "n/I");
         t.Branch("pt", pt, "pt[n]/F");
         for (int i = 0; i < 100000; ++i) {
            id = i;
            x = i * 0.5f;
            n = i % 9;
            for (int j = 0; j < n; ++j)
               pt[j] = j + 0.25f;
            t.Fill();
         }
         t.Write();
      });
   }
}
